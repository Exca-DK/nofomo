use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Dataset, GraphType, List, ListItem, ListState, Paragraph,
    Row, Table,
};
use ratatui::{Frame, text::Line};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tempo_agentic_domain::format_units_string;
use tempo_agentic_mcp::manifest_path;
use tokio::task::JoinHandle;

use crate::admin_client::Endpoint;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MARKET_INTERVAL: Duration = Duration::from_secs(5 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const MARKET_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Attaches to the daemon without opening its database or loading wallet configuration.
pub async fn run(config_path: &str) -> Result<()> {
    let mut client = DashboardClient::attach(config_path)
        .await
        .map_err(|_| anyhow!("daemon is not running"))?;
    let mut terminal = TerminalGuard::new()?;
    let mut next_poll = Instant::now() + POLL_INTERVAL;
    let mut next_market = Instant::now();

    loop {
        client.finish_market_refresh().await;
        terminal
            .terminal
            .draw(|frame| render(frame, &client.state))?;
        if event::poll(Duration::from_millis(100))?
            && let event::Event::Key(key) = event::read()?
        {
            match client.state.handle_key(key) {
                InputAction::Quit => break,
                InputAction::Refresh => {
                    next_poll = Instant::now();
                    next_market = Instant::now();
                }
                InputAction::MarketChanged => next_market = Instant::now(),
                InputAction::Continue => {}
            }
        }
        if Instant::now() >= next_poll {
            client.refresh().await;
            next_poll = Instant::now() + POLL_INTERVAL;
        }
        if Instant::now() >= next_market || client.state.market_dirty {
            client.start_market_refresh();
            next_market = Instant::now() + MARKET_INTERVAL;
        }
    }
    Ok(())
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode().context("cannot enable terminal raw mode")?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error).context("cannot enter alternate screen");
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
                Err(error).context("cannot initialize terminal")
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

struct DashboardClient {
    http: Client,
    market_http: Client,
    manifest: PathBuf,
    endpoint: Endpoint,
    reload_manifest: bool,
    state: AppState,
    market_request: Option<JoinHandle<(String, Result<MarketChartView>)>>,
}

impl DashboardClient {
    async fn attach(config_path: impl AsRef<Path>) -> Result<Self> {
        let database = locate_database(config_path.as_ref())?;
        let manifest = manifest_path(&database);
        let endpoint = Endpoint::read(&manifest)?;
        let http = Client::builder().timeout(HTTP_TIMEOUT).build()?;
        let market_http = Client::builder().timeout(MARKET_HTTP_TIMEOUT).build()?;
        let snapshot = endpoint.fetch(&http).await?;
        let mut state = AppState::default();
        state.apply(snapshot);
        Ok(Self {
            http,
            market_http,
            manifest,
            endpoint,
            reload_manifest: false,
            state,
            market_request: None,
        })
    }

    async fn refresh(&mut self) {
        if self.reload_manifest {
            match Endpoint::read(&self.manifest) {
                Ok(endpoint) => self.endpoint = endpoint,
                Err(error) => {
                    self.state.disconnect(error.to_string());
                    return;
                }
            }
        }
        match self.endpoint.fetch(&self.http).await {
            Ok(snapshot) => {
                self.state.apply(snapshot);
                self.reload_manifest = false;
            }
            Err(error) => {
                self.state.disconnect(error.to_string());
                self.reload_manifest = true;
            }
        }
    }

    fn start_market_refresh(&mut self) {
        let Some(strategy_id) = self.state.selected_strategy.clone() else {
            self.state.market_dirty = false;
            self.state.market_loading = false;
            return;
        };
        if self
            .market_request
            .as_ref()
            .is_some_and(|request| !request.is_finished())
            && !self.state.market_dirty
        {
            return;
        }
        if let Some(request) = self.market_request.take() {
            request.abort();
        }
        self.state.market_dirty = false;
        self.state.market_loading = true;
        let endpoint = self.endpoint.clone();
        let http = self.market_http.clone();
        let request_id = strategy_id.clone();
        self.market_request = Some(tokio::spawn(async move {
            let result = endpoint.fetch_market(&http, &request_id).await;
            (request_id, result)
        }));
    }

    async fn finish_market_refresh(&mut self) {
        let Some(request) = self.market_request.as_ref() else {
            return;
        };
        if !request.is_finished() {
            return;
        }
        let request = self
            .market_request
            .take()
            .expect("finished market request still exists");
        match request.await {
            Ok((strategy_id, result)) => self.state.apply_market(&strategy_id, result),
            Err(error) if error.is_cancelled() => {}
            Err(error) => self
                .state
                .apply_market_error(format!("market request task failed: {error}")),
        }
    }
}

#[derive(Deserialize)]
struct DatabaseLocator {
    state_db_path: Option<PathBuf>,
}

pub(crate) fn locate_database(config_path: &Path) -> Result<PathBuf> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("cannot read config {}", config_path.display()))?;
    let locator: DatabaseLocator = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON in {}", config_path.display()))?;
    Ok(locator.state_db_path.unwrap_or_else(default_database_path))
}

fn default_database_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|path| path.join(".tempo-agentic/state.db"))
        .unwrap_or_else(|| PathBuf::from("/tmp/tempo-agentic.db"))
}

impl Endpoint {
    async fn fetch(&self, http: &Client) -> Result<DashboardSnapshot> {
        let response = http
            .get(self.url.join("dashboard")?)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("dashboard request failed")?;
        if response.status() == StatusCode::UNAUTHORIZED {
            bail!("dashboard authentication expired");
        }
        response
            .error_for_status()
            .context("dashboard request failed")?
            .json()
            .await
            .context("invalid dashboard response")
    }

    async fn fetch_market(&self, http: &Client, strategy_id: &str) -> Result<MarketChartView> {
        let response = http
            .post(self.url.join("dashboard/market")?)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "strategy_id": strategy_id }))
            .send()
            .await
            .context("market request failed")?;
        if response.status() == StatusCode::UNAUTHORIZED {
            bail!("dashboard authentication expired");
        }
        if !response.status().is_success() {
            let status = response.status();
            let body: ErrorResponse = response.json().await.unwrap_or_else(|_| ErrorResponse {
                error: "market data is unavailable".into(),
            });
            bail!("{status}: {}", body.error);
        }
        response.json().await.context("invalid market response")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DashboardSnapshot {
    version: String,
    started_at: i64,
    generated_at: i64,
    allow_broadcast: bool,
    strategies: Vec<StrategyView>,
    levels: Vec<LevelView>,
    orders: Vec<OrderView>,
    feeds: Vec<FeedView>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StrategyView {
    id: String,
    venue: String,
    chain: String,
    base_token: String,
    quote_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LevelView {
    id: String,
    strategy_id: String,
    side: String,
    token_in: String,
    token_out: String,
    trigger_price_usd: f64,
    price_pair: FeedPair,
    amount: String,
    amount_decimals: u8,
    runtime_state: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OrderView {
    id: String,
    level_id: String,
    status: String,
    phase: String,
    tx_hash: Option<String>,
    created_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FeedView {
    pair: FeedPair,
    health: String,
    last_tick: Option<TickView>,
    last_error: Option<FeedErrorView>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
struct FeedPair {
    chain_id: u64,
    token_address: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TickView {
    price_usd: f64,
    published_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FeedErrorView {
    category: String,
    message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MarketChartView {
    strategy_id: String,
    chain: String,
    base_token: String,
    quote_token: String,
    generated_at: i64,
    indexed_at: Option<i64>,
    pool: MarketPoolView,
    prices: Vec<PriceCandleView>,
    liquidity: Vec<LiquidityPointView>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MarketPoolView {
    id: String,
    fee_tier: String,
    tvl_usd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PriceCandleView {
    started_at: i64,
    open_usd: String,
    high_usd: String,
    low_usd: String,
    close_usd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LiquidityPointView {
    price_usd: String,
    active_liquidity: String,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Panel {
    #[default]
    Strategies,
    Levels,
    Market,
    Executions,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ChartMode {
    #[default]
    Price,
    Liquidity,
}

#[derive(Default)]
struct AppState {
    snapshot: Option<DashboardSnapshot>,
    selected_strategy: Option<String>,
    selected_level: Option<String>,
    disconnected: bool,
    connection_error: Option<String>,
    panel: Panel,
    chart_mode: ChartMode,
    market: Option<MarketChartView>,
    market_loading: bool,
    market_stale: bool,
    market_error: Option<String>,
    market_dirty: bool,
}

impl AppState {
    fn apply(&mut self, snapshot: DashboardSnapshot) {
        let old_strategy = self.selected_strategy.clone();
        self.selected_strategy = selected_id(
            self.selected_strategy.as_deref(),
            snapshot
                .strategies
                .iter()
                .map(|strategy| strategy.id.as_str()),
        );
        self.selected_level = selected_id(
            self.selected_level.as_deref(),
            snapshot
                .levels
                .iter()
                .filter(|level| {
                    Some(level.strategy_id.as_str()) == self.selected_strategy.as_deref()
                })
                .map(|level| level.id.as_str()),
        );
        if self.selected_strategy != old_strategy {
            self.market = None;
            self.market_error = None;
            self.market_stale = false;
            self.market_dirty = true;
        }
        self.snapshot = Some(snapshot);
        self.disconnected = false;
        self.connection_error = None;
    }

    fn disconnect(&mut self, error: String) {
        self.disconnected = true;
        self.connection_error = Some(error);
    }

    fn apply_market(&mut self, strategy_id: &str, result: Result<MarketChartView>) {
        if self.selected_strategy.as_deref() != Some(strategy_id) {
            return;
        }
        self.market_loading = false;
        match result {
            Ok(market) => {
                self.market = Some(market);
                self.market_stale = false;
                self.market_error = None;
            }
            Err(error) => self.apply_market_error(error.to_string()),
        }
    }

    fn apply_market_error(&mut self, error: String) {
        self.market_loading = false;
        self.market_stale = self.market.is_some();
        self.market_error = Some(error);
    }

    fn selected_feed(&self) -> Option<&FeedView> {
        let snapshot = self.snapshot.as_ref()?;
        let pair = &snapshot
            .levels
            .iter()
            .find(|level| Some(level.id.as_str()) == self.selected_level.as_deref())?
            .price_pair;
        snapshot.feeds.iter().find(|feed| &feed.pair == pair)
    }

    fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        if key.code == KeyCode::Char('q')
            || key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return InputAction::Quit;
        }
        match key.code {
            KeyCode::Char('r') => InputAction::Refresh,
            KeyCode::Char('l') => {
                self.chart_mode = match self.chart_mode {
                    ChartMode::Price => ChartMode::Liquidity,
                    ChartMode::Liquidity => ChartMode::Price,
                };
                InputAction::Continue
            }
            KeyCode::Tab => {
                self.panel = match self.panel {
                    Panel::Strategies => Panel::Levels,
                    Panel::Levels => Panel::Market,
                    Panel::Market => Panel::Executions,
                    Panel::Executions => Panel::Strategies,
                };
                InputAction::Continue
            }
            KeyCode::Up => {
                if self.move_selection(-1) {
                    InputAction::MarketChanged
                } else {
                    InputAction::Continue
                }
            }
            KeyCode::Down => {
                if self.move_selection(1) {
                    InputAction::MarketChanged
                } else {
                    InputAction::Continue
                }
            }
            _ => InputAction::Continue,
        }
    }

    fn move_selection(&mut self, delta: isize) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        let old_strategy = self.selected_strategy.clone();
        match self.panel {
            Panel::Strategies => {
                self.selected_strategy = move_id(
                    self.selected_strategy.as_deref(),
                    snapshot
                        .strategies
                        .iter()
                        .map(|strategy| strategy.id.as_str()),
                    delta,
                );
                self.selected_level = selected_id(
                    None,
                    snapshot
                        .levels
                        .iter()
                        .filter(|level| {
                            Some(level.strategy_id.as_str()) == self.selected_strategy.as_deref()
                        })
                        .map(|level| level.id.as_str()),
                );
            }
            Panel::Levels => {
                self.selected_level = move_id(
                    self.selected_level.as_deref(),
                    snapshot
                        .levels
                        .iter()
                        .filter(|level| {
                            Some(level.strategy_id.as_str()) == self.selected_strategy.as_deref()
                        })
                        .map(|level| level.id.as_str()),
                    delta,
                );
            }
            Panel::Market | Panel::Executions => {}
        }
        if self.selected_strategy != old_strategy {
            self.market = None;
            self.market_error = None;
            self.market_stale = false;
            self.market_dirty = true;
            true
        } else {
            false
        }
    }
}

fn selected_id<'a>(current: Option<&str>, ids: impl Iterator<Item = &'a str>) -> Option<String> {
    let ids = ids.collect::<Vec<_>>();
    current
        .filter(|current| ids.contains(current))
        .or_else(|| ids.first().copied())
        .map(str::to_owned)
}

fn move_id<'a>(
    current: Option<&str>,
    ids: impl Iterator<Item = &'a str>,
    delta: isize,
) -> Option<String> {
    let ids = ids.collect::<Vec<_>>();
    if ids.is_empty() {
        return None;
    }
    let current = current
        .and_then(|current| ids.iter().position(|id| *id == current))
        .unwrap_or(0);
    let next = (current as isize + delta).clamp(0, ids.len() as isize - 1) as usize;
    Some(ids[next].to_owned())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputAction {
    Continue,
    Refresh,
    MarketChanged,
    Quit,
}

fn render(frame: &mut Frame<'_>, state: &AppState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    frame.render_widget(header_widget(state), header);
    if body.width < 80 || body.height < 18 {
        render_single_panel(frame, state, body);
    } else {
        render_dashboard(frame, state, body);
    }
    frame.render_widget(
        Paragraph::new("Tab panel · ↑↓ navigate · l price/liquidity · r refresh · q detach")
            .block(Block::default().borders(Borders::ALL)),
        footer,
    );
}

fn header_widget(state: &AppState) -> Paragraph<'static> {
    let Some(snapshot) = &state.snapshot else {
        return Paragraph::new("connecting · waiting for daemon snapshot")
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default().borders(Borders::ALL).title("Dashboard"));
    };
    let connection = if state.disconnected {
        "disconnected · stale snapshot"
    } else {
        "connected"
    };
    let feed = state.selected_feed();
    let feed_status = feed.map_or("no feed", |feed| feed.health.as_str());
    let price = feed.and_then(|feed| feed.last_tick.as_ref()).map_or_else(
        || "no price".to_string(),
        |tick| format!("${:.4}", tick.price_usd),
    );
    Paragraph::new(format!(
        "{connection} · v{} · started {} · broadcast {} · feed {feed_status} · {price} · snapshot {}{}",
        snapshot.version,
        snapshot.started_at,
        if snapshot.allow_broadcast {
            "on"
        } else {
            "off"
        },
        snapshot.generated_at,
        feed.and_then(|feed| feed.last_error.as_ref()).map_or_else(
            String::new,
            |error| format!(" · {}: {}", error.category, error.message),
        ),
    ))
    .style(Style::default().fg(if state.disconnected {
        Color::Red
    } else if feed_status == "live" {
        Color::Green
    } else {
        Color::Yellow
    }))
    .block(Block::default().borders(Borders::ALL).title("Dashboard"))
}

fn render_dashboard(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let [top, executions] =
        Layout::vertical([Constraint::Min(8), Constraint::Length(9)]).areas(area);
    let [navigation, market] =
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).areas(top);
    let [strategies, levels] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)])
            .areas(navigation);
    render_strategies(frame, state, strategies);
    render_levels(frame, state, levels);
    render_market(frame, state, market);
    render_executions(frame, state, executions);
}

fn render_single_panel(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    match state.panel {
        Panel::Strategies => render_strategies(frame, state, area),
        Panel::Levels => render_levels(frame, state, area),
        Panel::Market => render_market(frame, state, area),
        Panel::Executions => render_executions(frame, state, area),
    }
}

fn render_strategies(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let strategies = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.strategies.as_slice())
        .unwrap_or_default();
    let items = strategies
        .iter()
        .map(|strategy| {
            ListItem::new(format!(
                "{} · {}/{}",
                strategy.id, strategy.base_token, strategy.quote_token
            ))
        })
        .collect::<Vec<_>>();
    let mut selection =
        ListState::default().with_selected(state.selected_strategy.as_ref().and_then(|selected| {
            strategies
                .iter()
                .position(|strategy| &strategy.id == selected)
        }));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .block(panel_block("Strategies", state.panel == Panel::Strategies)),
        area,
        &mut selection,
    );
}

fn render_levels(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let levels = state
        .snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .levels
                .iter()
                .filter(|level| {
                    Some(level.strategy_id.as_str()) == state.selected_strategy.as_deref()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let items = levels
        .iter()
        .map(|level| {
            ListItem::new(format!(
                "{} {} ${:.4} · {} {} [{}]",
                level.id,
                level.side,
                level.trigger_price_usd,
                level_amount(level),
                level.token_in,
                level.runtime_state
            ))
        })
        .collect::<Vec<_>>();
    let mut selection = ListState::default().with_selected(
        state
            .selected_level
            .as_ref()
            .and_then(|selected| levels.iter().position(|level| &level.id == selected)),
    );
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .block(panel_block("Levels", state.panel == Panel::Levels)),
        area,
        &mut selection,
    );
}

fn render_market(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let [description, chart] =
        Layout::vertical([Constraint::Length(7), Constraint::Min(3)]).areas(area);
    let strategy = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .strategies
            .iter()
            .find(|strategy| Some(strategy.id.as_str()) == state.selected_strategy.as_deref())
    });
    let level = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .levels
            .iter()
            .find(|level| Some(level.id.as_str()) == state.selected_level.as_deref())
    });
    let mut text = match (strategy, level) {
        (Some(strategy), Some(level)) => format!(
            "{} · {} · {}\n{}/{}\n{}: {} {} → {}\ntrigger ${:.4}",
            strategy.id,
            strategy.venue,
            strategy.chain,
            strategy.base_token,
            strategy.quote_token,
            level.side,
            level_amount(level),
            level.token_in,
            level.token_out,
            level.trigger_price_usd,
        ),
        (Some(strategy), None) => format!(
            "{} · {} · {}\n{}/{}\nno level selected",
            strategy.id, strategy.venue, strategy.chain, strategy.base_token, strategy.quote_token
        ),
        _ => "no strategies".to_string(),
    };
    if let Some(error) = &state.market_error {
        text.push_str(&format!("\nmarket: {error}"));
    }
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Market detail"),
        ),
        description,
    );

    let Some(market) = state
        .market
        .as_ref()
        .filter(|market| Some(market.strategy_id.as_str()) == state.selected_strategy.as_deref())
    else {
        let message = if state.market_loading {
            "loading market data from The Graph"
        } else {
            state
                .market_error
                .as_deref()
                .unwrap_or("market data not loaded")
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(if state.market_error.is_some() {
                    Color::Red
                } else {
                    Color::Yellow
                }))
                .block(market_block(state, "Market")),
            chart,
        );
        return;
    };
    match state.chart_mode {
        ChartMode::Price => render_price_chart(frame, state, market, chart),
        ChartMode::Liquidity => render_liquidity_chart(frame, state, market, chart),
    }
}

fn render_price_chart(
    frame: &mut Frame<'_>,
    state: &AppState,
    market: &MarketChartView,
    area: Rect,
) {
    let prices = market
        .prices
        .iter()
        .filter_map(|candle| {
            decimal_f64(&candle.close_usd).map(|price| (candle.started_at as f64, price))
        })
        .collect::<Vec<_>>();
    if prices.is_empty() {
        render_market_message(
            frame,
            state,
            area,
            "The Graph returned no 24h price history",
        );
        return;
    }
    let x_bounds = padded_bounds(prices.iter().map(|point| point.0), 0.0);
    let levels = active_levels(state);
    let y_bounds = padded_bounds(
        prices
            .iter()
            .map(|point| point.1)
            .chain(levels.iter().map(|level| level.trigger_price_usd)),
        0.02,
    );
    let level_lines = levels
        .iter()
        .map(|level| {
            (
                level_style(state, level),
                vec![
                    (x_bounds[0], level.trigger_price_usd),
                    (x_bounds[1], level.trigger_price_usd),
                ],
            )
        })
        .collect::<Vec<_>>();
    let mut datasets = vec![
        Dataset::default()
            .name("The Graph close")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Color::Cyan)
            .data(&prices),
    ];
    datasets.extend(level_lines.iter().map(|(style, points)| {
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(*style)
            .data(points)
    }));
    let title = format!(
        "Price · 24h · {} levels{}",
        levels.len(),
        market_suffix(state, market)
    );
    frame.render_widget(
        Chart::new(datasets)
            .block(market_block(state, title))
            .x_axis(
                Axis::default()
                    .title("time")
                    .bounds(x_bounds)
                    .labels(["-24h", "now"]),
            )
            .y_axis(Axis::default().title("USD").bounds(y_bounds).labels([
                format!("${:.4}", y_bounds[0]),
                format!("${:.4}", y_bounds[1]),
            ])),
        area,
    );
}

fn render_liquidity_chart(
    frame: &mut Frame<'_>,
    state: &AppState,
    market: &MarketChartView,
    area: Rect,
) {
    let raw = market
        .liquidity
        .iter()
        .filter_map(|point| {
            Some((
                decimal_f64(&point.price_usd)?,
                decimal_f64(&point.active_liquidity)?,
            ))
        })
        .collect::<Vec<_>>();
    let maximum = raw.iter().map(|point| point.1).fold(0.0, f64::max);
    if raw.is_empty() || maximum <= 0.0 {
        render_market_message(frame, state, area, "The Graph returned no active liquidity");
        return;
    }
    let points = raw
        .iter()
        .map(|(price, liquidity)| (*price, liquidity / maximum * 100.0))
        .collect::<Vec<_>>();
    let levels = active_levels(state);
    let x_bounds = padded_bounds(
        points
            .iter()
            .map(|point| point.0)
            .chain(levels.iter().map(|level| level.trigger_price_usd)),
        0.02,
    );
    let level_lines = levels
        .iter()
        .map(|level| {
            (
                level_style(state, level),
                vec![
                    (level.trigger_price_usd, 0.0),
                    (level.trigger_price_usd, 100.0),
                ],
            )
        })
        .collect::<Vec<_>>();
    let mut datasets = vec![
        Dataset::default()
            .name("active liquidity")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Color::Magenta)
            .data(&points),
    ];
    datasets.extend(level_lines.iter().map(|(style, points)| {
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(*style)
            .data(points)
    }));
    let title = format!(
        "Liquidity · pool {} · fee {} · TVL ${}{}",
        short_id(&market.pool.id),
        market.pool.fee_tier,
        market.pool.tvl_usd,
        market_suffix(state, market)
    );
    frame.render_widget(
        Chart::new(datasets)
            .block(market_block(state, title))
            .x_axis(Axis::default().title("price USD").bounds(x_bounds).labels([
                format!("${:.4}", x_bounds[0]),
                format!("${:.4}", x_bounds[1]),
            ]))
            .y_axis(
                Axis::default()
                    .title("relative")
                    .bounds([0.0, 100.0])
                    .labels(["0%", "100%"]),
            ),
        area,
    );
}

fn render_market_message(frame: &mut Frame<'_>, state: &AppState, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(message)
            .style(Style::default().fg(Color::Yellow))
            .block(market_block(state, "Market")),
        area,
    );
}

fn active_levels(state: &AppState) -> Vec<&LevelView> {
    state
        .snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .levels
                .iter()
                .filter(|level| {
                    Some(level.strategy_id.as_str()) == state.selected_strategy.as_deref()
                        && level.runtime_state != "filled"
                })
                .collect()
        })
        .unwrap_or_default()
}

fn level_style(state: &AppState, level: &LevelView) -> Style {
    let color = if state.selected_level.as_deref() == Some(level.id.as_str()) {
        Color::Yellow
    } else if level.side == "buy" {
        Color::Green
    } else {
        Color::Red
    };
    Style::default().fg(color)
}

fn level_amount(level: &LevelView) -> String {
    format_units_string(&level.amount, level.amount_decimals)
        .unwrap_or_else(|_| level.amount.clone())
}

fn decimal_f64(value: &str) -> Option<f64> {
    let value = value.parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn padded_bounds(values: impl Iterator<Item = f64>, padding: f64) -> [f64; 2] {
    let (mut minimum, mut maximum) = values.fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(minimum, maximum), value| (minimum.min(value), maximum.max(value)),
    );
    if !minimum.is_finite() || !maximum.is_finite() {
        return [0.0, 1.0];
    }
    if maximum <= minimum {
        let pad = minimum.abs().max(1.0) * 0.01;
        minimum -= pad;
        maximum += pad;
    } else {
        let pad = (maximum - minimum) * padding;
        minimum -= pad;
        maximum += pad;
    }
    [minimum, maximum]
}

fn short_id(id: &str) -> &str {
    id.get(..id.len().min(10)).unwrap_or(id)
}

fn market_suffix(state: &AppState, market: &MarketChartView) -> String {
    let freshness = if state.market_stale {
        " · stale"
    } else if state.market_loading {
        " · refreshing"
    } else {
        ""
    };
    let indexed = market
        .indexed_at
        .map_or_else(String::new, |at| format!(" · indexed {at}"));
    format!("{freshness}{indexed}")
}

fn market_block(state: &AppState, title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if state.panel == Panel::Market {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        })
}

fn render_executions(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let rows = state
        .snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .orders
                .iter()
                .filter(|order| Some(order.level_id.as_str()) == state.selected_level.as_deref())
                .map(|order| {
                    Row::new([
                        Cell::from(order.id.clone()),
                        Cell::from(order.created_at.to_string()),
                        Cell::from(order.status.clone()),
                        Cell::from(order.phase.clone()),
                        Cell::from(order.tx_hash.clone().unwrap_or_else(|| "—".into())),
                    ])
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(13),
                Constraint::Length(14),
                Constraint::Min(8),
            ],
        )
        .header(Row::new(["id", "created", "status", "phase", "tx"]))
        .block(panel_block("Executions", state.panel == Panel::Executions)),
        area,
    );
}

fn panel_block(title: &'static str, selected: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title))
        .border_style(if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn locator_reads_only_database_path_and_ignores_missing_secrets() {
        let config = temp_path("locator.json");
        let database = temp_path("wanted.db");
        std::fs::write(
            &config,
            serde_json::json!({
                "state_db_path": database,
                "evm": { "keystore_path": "/does/not/exist", "password_file": "/secret" },
                "uniswap": "not-even-the-full-config-shape"
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(locate_database(&config).unwrap(), database);
        let _ = std::fs::remove_file(config);
    }

    #[tokio::test]
    async fn initial_attach_requires_a_live_authenticated_snapshot() {
        let database = temp_path("absent-daemon.db");
        let config = write_config(&database);

        let error = match DashboardClient::attach(&config).await {
            Ok(_) => panic!("attach unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("cannot read admin manifest"));

        let _ = std::fs::remove_file(config);
    }

    #[tokio::test]
    async fn reconnect_reloads_manifest_url_and_token_after_real_server_restart() {
        let database = temp_path("restart.db");
        let config = write_config(&database);
        let manifest = manifest_path(&database);
        let (first_url, first) = snapshot_server("old-token", snapshot_json(1)).await;
        write_manifest(&manifest, &first_url, "old-token");

        let mut client = DashboardClient::attach(&config).await.unwrap();
        assert_eq!(first.await.unwrap(), "Bearer old-token");
        assert_eq!(client.state.snapshot.as_ref().unwrap().generated_at, 1);

        client.refresh().await;
        assert!(client.state.disconnected);
        assert_eq!(client.state.snapshot.as_ref().unwrap().generated_at, 1);

        let (second_url, second) = snapshot_server("new-token", snapshot_json(2)).await;
        write_manifest(&manifest, &second_url, "new-token");
        client.refresh().await;

        assert_eq!(second.await.unwrap(), "Bearer new-token");
        assert!(!client.state.disconnected);
        assert_eq!(client.state.snapshot.as_ref().unwrap().generated_at, 2);
        for path in [config, manifest] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[tokio::test]
    async fn market_request_uses_the_selected_strategy_and_bearer_token() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8_192];
            let count = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("POST /dashboard/market "));
            assert!(request.contains("authorization: Bearer token"));
            assert!(request.contains(r#""strategy_id":"s-1""#));
            let body = serde_json::to_vec(&market("s-1")).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        });
        let endpoint = Endpoint {
            url: format!("http://{address}/").parse().unwrap(),
            token: "token".into(),
        };

        let response = endpoint.fetch_market(&Client::new(), "s-1").await.unwrap();

        assert_eq!(response.strategy_id, "s-1");
        server.await.unwrap();
    }

    #[test]
    fn selection_survives_reordering_and_a_market_change_is_marked_dirty() {
        let mut state = AppState::default();
        let mut snapshot = snapshot(0);
        state.apply(snapshot.clone());
        state.market_dirty = false;
        state.selected_strategy = Some("s-2".into());
        state.selected_level = Some("l-2".into());
        snapshot.strategies.reverse();
        snapshot.levels.reverse();
        state.apply(snapshot);

        assert_eq!(state.selected_strategy.as_deref(), Some("s-2"));
        assert_eq!(state.selected_level.as_deref(), Some("l-2"));
        state.panel = Panel::Strategies;
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            InputAction::MarketChanged
        );
        assert!(state.market_dirty);
    }

    #[test]
    fn navigation_and_quit_are_local_state_only() {
        let mut state = AppState::default();
        state.apply(snapshot(1));
        assert_eq!(state.selected_strategy.as_deref(), Some("s-1"));
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            InputAction::MarketChanged
        );
        assert_eq!(state.selected_strategy.as_deref(), Some("s-2"));
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            InputAction::Quit
        );
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            InputAction::Quit
        );
    }

    #[test]
    fn selected_level_uses_its_own_feed_and_exact_amount() {
        let mut state = AppState::default();
        state.apply(snapshot(1));
        state.selected_strategy = Some("s-2".into());
        state.selected_level = Some("l-2".into());

        let feed = state.selected_feed().unwrap();
        assert_eq!(feed.pair.token_address, "0xbtc");
        let text = render_text(&state, 140, 30);
        assert!(text.contains("$90010.0000"));
        assert!(text.contains("feed degraded"));
        assert!(text.contains("1.25 WBTC"));
        assert!(!text.contains("$3010.0000"));
    }

    #[test]
    fn toggles_between_price_and_liquidity_for_the_selected_market() {
        let mut state = AppState::default();
        state.apply(snapshot(1));
        state.market = Some(market("s-1"));
        state.market_dirty = false;

        let price = render_text(&state, 140, 30);
        assert!(price.contains("Price"));
        assert!(price.contains("24h"));

        state.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        let liquidity = render_text(&state, 140, 30);
        assert!(liquidity.contains("Liquidity"));
        assert!(liquidity.contains("TVL $1000000"));

        state.apply_market_error("The Graph is down".into());
        let stale = render_text(&state, 140, 30);
        assert!(stale.contains("stale"));
        assert!(stale.contains("market: The Graph is down"));
    }

    #[test]
    fn renders_empty_connecting_live_degraded_disconnected_and_small_terminal() {
        let empty = AppState::default();
        assert!(render_text(&empty, 100, 30).contains("connecting"));

        let mut connecting = AppState::default();
        let mut connecting_snapshot = snapshot(1);
        connecting_snapshot.feeds[0].health = "connecting".into();
        connecting.apply(connecting_snapshot);
        assert!(render_text(&connecting, 100, 30).contains("connecting"));

        let mut live = AppState::default();
        live.apply(snapshot(1));
        let text = render_text(&live, 100, 30);
        assert!(text.contains("connected"));
        assert!(text.contains("live"));
        assert!(text.contains("Strategies"));
        assert!(text.contains("Executions"));

        let mut degraded = AppState::default();
        let mut degraded_snapshot = snapshot(1);
        degraded_snapshot.feeds[0].health = "degraded".into();
        degraded.apply(degraded_snapshot);
        assert!(render_text(&degraded, 100, 30).contains("degraded"));

        degraded.disconnect("server stopped".into());
        assert!(render_text(&degraded, 100, 30).contains("disconnected"));

        let small = render_text(&live, 50, 12);
        assert!(small.contains("Strategies"));
        assert!(!small.contains("Executions"));
    }

    fn render_text(state: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    async fn snapshot_server(
        expected_token: &'static str,
        body: serde_json::Value,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let count = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            let authorization = request
                .lines()
                .find_map(|line| line.strip_prefix("authorization: "))
                .unwrap_or_default()
                .to_string();
            assert_eq!(authorization, format!("Bearer {expected_token}"));
            let body = serde_json::to_vec(&body).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
            authorization
        });
        (format!("http://{address}/"), handle)
    }

    fn write_config(database: &Path) -> PathBuf {
        let path = database.with_extension("config.json");
        std::fs::write(
            &path,
            serde_json::json!({ "state_db_path": database }).to_string(),
        )
        .unwrap();
        path
    }

    fn write_manifest(path: &Path, url: &str, token: &str) {
        std::fs::write(
            path,
            serde_json::json!({ "url": url, "token": token }).to_string(),
        )
        .unwrap();
    }

    fn snapshot_json(generated_at: i64) -> serde_json::Value {
        serde_json::to_value(snapshot(generated_at)).unwrap()
    }

    fn snapshot(generated_at: i64) -> DashboardSnapshot {
        DashboardSnapshot {
            version: "test".into(),
            started_at: 1,
            generated_at,
            allow_broadcast: false,
            strategies: vec![
                StrategyView {
                    id: "s-1".into(),
                    venue: "uniswap".into(),
                    chain: "base".into(),
                    base_token: "WETH".into(),
                    quote_token: "USDC".into(),
                },
                StrategyView {
                    id: "s-2".into(),
                    venue: "uniswap".into(),
                    chain: "base".into(),
                    base_token: "WBTC".into(),
                    quote_token: "USDC".into(),
                },
            ],
            levels: vec![
                LevelView {
                    id: "l-1".into(),
                    strategy_id: "s-1".into(),
                    side: "buy".into(),
                    token_in: "USDC".into(),
                    token_out: "WETH".into(),
                    trigger_price_usd: 3_000.0,
                    price_pair: FeedPair {
                        chain_id: 8453,
                        token_address: "0xbase".into(),
                    },
                    amount: "2500000".into(),
                    amount_decimals: 6,
                    runtime_state: "armed".into(),
                },
                LevelView {
                    id: "l-2".into(),
                    strategy_id: "s-2".into(),
                    side: "sell".into(),
                    token_in: "WBTC".into(),
                    token_out: "USDC".into(),
                    trigger_price_usd: 90_000.0,
                    price_pair: FeedPair {
                        chain_id: 8453,
                        token_address: "0xbtc".into(),
                    },
                    amount: "125000000".into(),
                    amount_decimals: 8,
                    runtime_state: "cooldown".into(),
                },
            ],
            orders: vec![OrderView {
                id: "o-1".into(),
                level_id: "l-1".into(),
                status: "filled".into(),
                phase: "filled".into(),
                tx_hash: Some("0xabc".into()),
                created_at: 1,
            }],
            feeds: vec![
                FeedView {
                    pair: FeedPair {
                        chain_id: 8453,
                        token_address: "0xbase".into(),
                    },
                    health: "live".into(),
                    last_tick: Some(TickView {
                        price_usd: 3_010.0,
                        published_at: generated_at,
                    }),
                    last_error: None,
                },
                FeedView {
                    pair: FeedPair {
                        chain_id: 8453,
                        token_address: "0xbtc".into(),
                    },
                    health: "degraded".into(),
                    last_tick: Some(TickView {
                        price_usd: 90_010.0,
                        published_at: generated_at,
                    }),
                    last_error: Some(FeedErrorView {
                        category: "source_error".into(),
                        message: "price source reported an error".into(),
                    }),
                },
            ],
        }
    }

    fn market(strategy_id: &str) -> MarketChartView {
        MarketChartView {
            strategy_id: strategy_id.into(),
            chain: "base".into(),
            base_token: "WETH".into(),
            quote_token: "USDC".into(),
            generated_at: 1,
            indexed_at: Some(1),
            pool: MarketPoolView {
                id: "0xpool".into(),
                fee_tier: "500".into(),
                tvl_usd: "1000000".into(),
            },
            prices: vec![
                PriceCandleView {
                    started_at: 1,
                    open_usd: "2990".into(),
                    high_usd: "3010".into(),
                    low_usd: "2980".into(),
                    close_usd: "3000".into(),
                },
                PriceCandleView {
                    started_at: 2,
                    open_usd: "3000".into(),
                    high_usd: "3020".into(),
                    low_usd: "2990".into(),
                    close_usd: "3010".into(),
                },
            ],
            liquidity: vec![
                LiquidityPointView {
                    price_usd: "2900".into(),
                    active_liquidity: "100".into(),
                },
                LiquidityPointView {
                    price_usd: "3100".into(),
                    active_liquidity: "200".into(),
                },
            ],
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "tempo-dashboard-{}-{unique}-{name}",
            std::process::id()
        ))
    }
}
