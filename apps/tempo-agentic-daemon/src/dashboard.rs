use std::collections::{HashMap, VecDeque};
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
use ratatui::widgets::{
    Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Sparkline, Table,
};
use ratatui::{Frame, text::Line};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tempo_agentic_mcp::manifest_path;

use crate::admin_client::Endpoint;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const HISTORY_LIMIT: usize = 120;

/// Attaches to the daemon without opening its database or loading wallet configuration.
pub async fn run(config_path: &str) -> Result<()> {
    let mut client = DashboardClient::attach(config_path)
        .await
        .map_err(|_| anyhow!("daemon is not running"))?;
    let mut terminal = TerminalGuard::new()?;
    let mut next_poll = Instant::now() + POLL_INTERVAL;

    loop {
        terminal
            .terminal
            .draw(|frame| render(frame, &client.state))?;
        if event::poll(Duration::from_millis(100))?
            && let event::Event::Key(key) = event::read()?
        {
            match client.state.handle_key(key) {
                InputAction::Quit => break,
                InputAction::Refresh => next_poll = Instant::now(),
                InputAction::Continue => {}
            }
        }
        if Instant::now() >= next_poll {
            client.refresh().await;
            next_poll = Instant::now() + POLL_INTERVAL;
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
    manifest: PathBuf,
    endpoint: Endpoint,
    reload_manifest: bool,
    state: AppState,
}

impl DashboardClient {
    async fn attach(config_path: impl AsRef<Path>) -> Result<Self> {
        let database = locate_database(config_path.as_ref())?;
        let manifest = manifest_path(&database);
        let endpoint = Endpoint::read(&manifest)?;
        let http = Client::builder().timeout(HTTP_TIMEOUT).build()?;
        let snapshot = endpoint.fetch(&http).await?;
        let mut state = AppState::default();
        state.apply(snapshot);
        Ok(Self {
            http,
            manifest,
            endpoint,
            reload_manifest: false,
            state,
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
}

#[derive(Deserialize)]
struct DatabaseLocator {
    state_db_path: Option<PathBuf>,
}

fn locate_database(config_path: &Path) -> Result<PathBuf> {
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Panel {
    #[default]
    Strategies,
    Levels,
    Executions,
}

#[derive(Default)]
struct AppState {
    snapshot: Option<DashboardSnapshot>,
    selected_strategy: Option<String>,
    selected_level: Option<String>,
    histories: HashMap<FeedPair, VecDeque<TickView>>,
    disconnected: bool,
    connection_error: Option<String>,
    panel: Panel,
}

impl AppState {
    fn apply(&mut self, snapshot: DashboardSnapshot) {
        for feed in &snapshot.feeds {
            if let Some(tick) = &feed.last_tick {
                let history = self.histories.entry(feed.pair.clone()).or_default();
                if history.back().map(|old| old.published_at) != Some(tick.published_at) {
                    if history.len() == HISTORY_LIMIT {
                        history.pop_front();
                    }
                    history.push_back(tick.clone());
                }
            }
        }
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
        self.snapshot = Some(snapshot);
        self.disconnected = false;
        self.connection_error = None;
    }

    fn disconnect(&mut self, error: String) {
        self.disconnected = true;
        self.connection_error = Some(error);
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
            KeyCode::Tab => {
                self.panel = match self.panel {
                    Panel::Strategies => Panel::Levels,
                    Panel::Levels => Panel::Executions,
                    Panel::Executions => Panel::Strategies,
                };
                InputAction::Continue
            }
            KeyCode::Up => {
                self.move_selection(-1);
                InputAction::Continue
            }
            KeyCode::Down => {
                self.move_selection(1);
                InputAction::Continue
            }
            _ => InputAction::Continue,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
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
            Panel::Executions => {}
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
        Paragraph::new("Tab panel · ↑↓ navigate · r refresh · q detach")
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
    let [strategies, levels, detail] = Layout::horizontal([
        Constraint::Percentage(24),
        Constraint::Percentage(36),
        Constraint::Percentage(40),
    ])
    .areas(top);
    render_strategies(frame, state, strategies);
    render_levels(frame, state, levels);
    render_detail(frame, state, detail);
    render_executions(frame, state, executions);
}

fn render_single_panel(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    match state.panel {
        Panel::Strategies => render_strategies(frame, state, area),
        Panel::Levels => render_levels(frame, state, area),
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
                "{} {} ${:.4} [{}]",
                level.id, level.side, level.trigger_price_usd, level.runtime_state
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

fn render_detail(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
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
    let text = match (strategy, level) {
        (Some(strategy), Some(level)) => format!(
            "{} · {} · {}\n{}/{}\n{}: {} → {}\ntrigger ${:.4}",
            strategy.id,
            strategy.venue,
            strategy.chain,
            strategy.base_token,
            strategy.quote_token,
            level.side,
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
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Market detail"),
        ),
        description,
    );

    let values = state
        .selected_feed()
        .and_then(|feed| state.histories.get(&feed.pair))
        .map(spark_values)
        .unwrap_or_default();
    frame.render_widget(
        Sparkline::default().data(&values).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Session price history"),
        ),
        chart,
    );
}

fn spark_values(history: &VecDeque<TickView>) -> Vec<u64> {
    let min = history
        .iter()
        .map(|tick| tick.price_usd)
        .fold(f64::INFINITY, f64::min);
    let max = history
        .iter()
        .map(|tick| tick.price_usd)
        .fold(f64::NEG_INFINITY, f64::max);
    history
        .iter()
        .map(|tick| {
            if max > min {
                ((tick.price_usd - min) / (max - min) * 100.0) as u64
            } else {
                50
            }
        })
        .collect()
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

    #[test]
    fn history_is_bounded_deduplicated_and_selection_survives_reordering() {
        let mut state = AppState::default();
        let mut snapshot = snapshot(0);
        state.apply(snapshot.clone());
        state.selected_strategy = Some("s-2".into());
        state.selected_level = Some("l-2".into());
        for published_at in 0..HISTORY_LIMIT as i64 + 10 {
            snapshot.generated_at = published_at;
            snapshot.feeds[0].last_tick = Some(TickView {
                price_usd: 100.0 + published_at as f64,
                published_at,
            });
            state.apply(snapshot.clone());
            state.apply(snapshot.clone());
        }
        snapshot.strategies.reverse();
        snapshot.levels.reverse();
        state.apply(snapshot);

        let history = &state.histories[&FeedPair {
            chain_id: 8453,
            token_address: "0xbase".into(),
        }];
        assert_eq!(history.len(), HISTORY_LIMIT);
        assert_eq!(state.selected_strategy.as_deref(), Some("s-2"));
        assert_eq!(state.selected_level.as_deref(), Some("l-2"));
    }

    #[test]
    fn navigation_and_quit_are_local_state_only() {
        let mut state = AppState::default();
        state.apply(snapshot(1));
        assert_eq!(state.selected_strategy.as_deref(), Some("s-1"));
        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
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
    fn selected_level_uses_its_own_feed_and_history() {
        let mut state = AppState::default();
        state.apply(snapshot(1));
        state.selected_strategy = Some("s-2".into());
        state.selected_level = Some("l-2".into());

        let feed = state.selected_feed().unwrap();
        assert_eq!(feed.pair.token_address, "0xbtc");
        assert_eq!(
            state.histories[&feed.pair].back().unwrap().price_usd,
            90_010.0
        );
        let text = render_text(&state, 140, 30);
        assert!(text.contains("$90010.0000"));
        assert!(text.contains("feed degraded"));
        assert!(!text.contains("$3010.0000"));
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
