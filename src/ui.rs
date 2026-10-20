use std::collections::VecDeque;
use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::U256;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use log::{LevelFilter, Log, Metadata, Record};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;
use ratatui::Terminal;

use crate::account::{Account, AccountStore, Position};

const LOG_CAP: usize = 500;

#[derive(Clone)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
    mirror_stderr: Arc<AtomicBool>,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::new())),
            mirror_stderr: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn install(self) -> Result<()> {
        let filter = std::env::var("RUST_LOG")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(LevelFilter::Info);
        log::set_boxed_logger(Box::new(BufferLogger {
            buffer: self,
            filter,
        }))
        .context("install log buffer")?;
        log::set_max_level(filter);
        Ok(())
    }

    pub fn quiet_stderr(&self) {
        self.mirror_stderr.store(false, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn push(&self, line: String) {
        if self.mirror_stderr.load(Ordering::Relaxed) {
            let _ = writeln!(io::stderr(), "{line}");
        }
        if let Ok(mut lines) = self.lines.lock() {
            if lines.len() >= LOG_CAP {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    }
}

struct BufferLogger {
    buffer: LogBuffer,
    filter: LevelFilter,
}

impl Log for BufferLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.filter
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let target = record.target();
        let short = target.rsplit("::").next().unwrap_or(target);
        self.buffer.push(format!(
            "[{}] {}: {}",
            record.level(),
            short,
            record.args()
        ));
    }

    fn flush(&self) {}
}

pub struct AppUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    logs: LogBuffer,
}

impl AppUi {
    pub fn enter(logs: LogBuffer) -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("create terminal")?;
        logs.quiet_stderr();
        Ok(Self { terminal, logs })
    }

    pub fn draw(&mut self, accounts: &AccountStore, last_px: Option<f64>) -> Result<()> {
        let account = accounts
            .try_read()
            .ok()
            .and_then(|guard| guard.clone());
        let log_lines = self.logs.snapshot();
        self.terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(14), Constraint::Min(5)])
                .split(frame.area());

            render_account_store(frame, chunks[0], account.as_ref(), last_px);

            let items: Vec<ListItem> = log_lines
                .iter()
                .rev()
                .take(chunks[1].height.saturating_sub(2) as usize)
                .rev()
                .map(|line| ListItem::new(Line::from(log_line_spans(line))))
                .collect();
            let logs = List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Logs (q / Ctrl-C to quit) "),
            );
            frame.render_widget(logs, chunks[1]);
        })?;
        Ok(())
    }

    pub fn poll_quit(&self) -> Result<bool> {
        while event::poll(Duration::ZERO)? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.code == KeyCode::Char('q')
                || key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Drop for AppUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

fn render_account_store(
    frame: &mut Frame<'_>,
    area: Rect,
    account: Option<&Account>,
    last_px: Option<f64>,
) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" AccountStore ")
        .title_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let mark_s = fmt_px(last_px);
    let Some(account) = account else {
        frame.render_widget(Paragraph::new("waiting for account snapshot"), inner);
        return;
    };

    let scale = account.token_scale.max(1);
    let round = round_stats(account, last_px);
    let pos_rows = account.positions.len().max(1) as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(pos_rows),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(inner);

    let farm_s = if round.ok {
        format!("{:.2}", round.farm)
    } else {
        "-".to_string()
    };
    let cost_s = if round.ok {
        if round.margin > 0.0 {
            format!(
                "{:.2} ({:.2}%)",
                round.cost,
                round.cost / round.margin * 100.0
            )
        } else {
            format!("{:.2}", round.cost)
        }
    } else {
        "-".to_string()
    };
    let raw_s = if round.ok {
        format!("{:.2}", round.raw)
    } else {
        "-".to_string()
    };
    frame.render_widget(
        six_col_table(
            ["available", "paper", "staked", "claimable", "", ""],
            [
                human_amount(account.available, scale),
                human_amount(account.paper.balance, scale),
                human_amount(account.paper.staked, scale),
                human_amount(account.paper.claimable, scale),
                String::new(),
                String::new(),
            ],
        ),
        chunks[0],
    );

    frame.render_widget(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );

    frame.render_widget(
        six_col_table(
            ["entry", "close", "mark", "raw", "cost", "farm"],
            [
                join_prices(account.positions.iter().map(|p| p.entry)),
                join_prices(account.positions.iter().map(|p| p.bust)),
                mark_s,
                raw_s,
                cost_s,
                farm_s,
            ],
        ),
        chunks[2],
    );

    let pos_table_rows: Vec<Row<'_>> = if account.positions.is_empty() {
        vec![six_cells(["-", "-", "-", "-", "-", "-"])]
    } else {
        account
            .positions
            .iter()
            .map(|pos| {
                let side = if pos.is_long { "long" } else { "short" };
                let pnl_s = unrealized_pnl(pos, last_px)
                    .map(|pnl| format!("{pnl:.2}"))
                    .unwrap_or_else(|| "-".to_string());
                six_cells([
                    pos.id.as_str(),
                    side,
                    &format!("{:.2}", pos.margin),
                    &format!("{:.2}", pos.size),
                    &format!("{:.2}", pos.notional),
                    &pnl_s,
                ])
            })
            .collect()
    };
    frame.render_widget(
        Table::new(pos_table_rows, six_col_widths()).column_spacing(1),
        chunks[3],
    );

    frame.render_widget(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
        chunks[4],
    );

    let inflight_s = if account.inflight.is_empty() {
        "none".to_string()
    } else {
        account
            .inflight
            .iter()
            .map(|op| op.action.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    frame.render_widget(
        six_col_table(
            ["inflight", "", "", "", "", ""],
            [&inflight_s, "", "", "", "", ""],
        ),
        chunks[5],
    );
}

fn six_col_widths() -> [Constraint; 6] {
    [
        Constraint::Percentage(17),
        Constraint::Percentage(16),
        Constraint::Percentage(17),
        Constraint::Percentage(16),
        Constraint::Percentage(17),
        Constraint::Percentage(17),
    ]
}

fn six_cells(values: [&str; 6]) -> Row<'static> {
    Row::new(values.map(|v| v.to_string()))
}

fn six_col_table(headers: [&str; 6], values: [impl Into<String>; 6]) -> Table<'static> {
    let header = Row::new(headers.map(|h| h.to_string()))
        .style(Style::default().add_modifier(Modifier::BOLD));
    let row = Row::new(values.map(|v| v.into()));
    Table::new(vec![row], six_col_widths())
        .header(header)
        .column_spacing(1)
}

fn join_prices(prices: impl Iterator<Item = f64>) -> String {
    let parts: Vec<String> = prices.map(fmt_num).collect();
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join("/")
    }
}

struct RoundStats {
    ok: bool,
    raw: f64,
    settle: f64,
    cost: f64,
    farm: f64,
    margin: f64,
}

fn round_stats(account: &Account, last_px: Option<f64>) -> RoundStats {
    let mut stats = RoundStats {
        ok: true,
        raw: 0.0,
        settle: 0.0,
        cost: 0.0,
        farm: 0.0,
        margin: 0.0,
    };
    if account.positions.is_empty() {
        stats.ok = false;
        return stats;
    }
    for pos in &account.positions {
        stats.margin += pos.margin.max(0.0);
        match (unrealized_pnl(pos, last_px), settle_pnl(pos, last_px)) {
            (Some(raw), Some(settle)) => {
                stats.raw += raw;
                stats.settle += settle;
                stats.cost += (raw - settle).max(0.0);
                stats.farm += expected_paper(raw, pos.margin, account.mint_rate_raw);
            }
            _ => stats.ok = false,
        }
    }
    stats
}

fn fmt_px(mark: Option<f64>) -> String {
    mark.filter(|px| px.is_finite() && *px > 0.0)
        .map(|px| format!("{px:.2}"))
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_num(value: f64) -> String {
    if value.is_finite() && value > 0.0 {
        format!("{value:.2}")
    } else {
        "-".to_string()
    }
}

const PPM: f64 = 1_000_000.0;
const DEADBAND_PPM: f64 = 20.0;
const WIN_FEE_PPM: f64 = 20_000.0;
const LOSS_FEE_PPM: f64 = 20_000.0;
const BASE_RATE_PPM: f64 = 100_000.0;
const RATE_MULTIPLIER: f64 = 15_000.0;
const POSITION_MULTIPLIER_PPM: f64 = 814_598_000.0;
const REFERENCE_NOTIONAL_RAW: f64 = 100_000_000_000.0;

fn unrealized_pnl(pos: &Position, mark: Option<f64>) -> Option<f64> {
    let mark = mark.filter(|px| px.is_finite() && *px > 0.0)?;
    if !(pos.entry.is_finite() && pos.entry > 0.0 && pos.notional.is_finite() && pos.notional > 0.0)
    {
        return None;
    }
    let delta = if pos.is_long {
        mark - pos.entry
    } else {
        pos.entry - mark
    };
    Some(pos.notional * delta / pos.entry)
}

fn expected_paper(raw_pnl: f64, margin: f64, mint_rate_raw: u64) -> f64 {
    if !(raw_pnl < 0.0) || !(margin > 0.0) || mint_rate_raw == 0 {
        return 0.0;
    }
    let loss = (-raw_pnl).min(margin);
    let mint_basis = loss * (PPM - LOSS_FEE_PPM) / PPM;
    mint_basis * (mint_rate_raw as f64) / PPM
}

fn settle_pnl(pos: &Position, mark: Option<f64>) -> Option<f64> {
    let raw = unrealized_pnl(pos, mark)?;
    if !(raw > 0.0) {
        return Some(raw);
    }
    let mark = mark.filter(|px| px.is_finite() && *px > 0.0)?;
    let band = pos.notional * DEADBAND_PPM / PPM;
    let gain_db = (raw - band).max(0.0);
    if gain_db <= 0.0 {
        return Some(0.0);
    }
    let entry_band = pos.entry * DEADBAND_PPM / PPM;
    let exit_adj = if pos.is_long {
        mark - entry_band
    } else {
        mark + entry_band
    };
    let move_ppm = (exit_adj - pos.entry).abs() * PPM / pos.entry;
    let scale = impact_scale_ppm(move_ppm);
    let gain_scaled = gain_db * scale / PPM;
    let fee = gain_scaled * WIN_FEE_PPM / PPM;
    Some(gain_scaled - fee)
}

fn impact_scale_ppm(move_ppm: f64) -> f64 {
    if !(move_ppm > 0.0) {
        return 0.0;
    }
    let term1 = PPM * PPM / (move_ppm * RATE_MULTIPLIER);
    let term2 = REFERENCE_NOTIONAL_RAW * PPM / (move_ppm * POSITION_MULTIPLIER_PPM);
    let denom = PPM + term1 + term2;
    if !(denom > 0.0) {
        return 0.0;
    }
    (PPM - BASE_RATE_PPM) * PPM / denom
}

fn human_amount(raw: U256, scale: u128) -> String {
    let scale = U256::from(scale);
    if scale.is_zero() {
        return raw.to_string();
    }
    let whole = raw / scale;
    let frac = raw % scale;
    let frac_width = scale.to_string().len().saturating_sub(1);
    if frac.is_zero() || frac_width == 0 {
        return whole.to_string();
    }
    let mut frac_s = format!("{frac:0>width$}", width = frac_width);
    while frac_s.ends_with('0') {
        frac_s.pop();
    }
    if frac_s.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac_s}")
    }
}

fn log_line_spans(line: &str) -> Vec<Span<'static>> {
    let style = if line.contains("[ERROR]") {
        Style::default().fg(Color::Red)
    } else if line.contains("[WARN]") {
        Style::default().fg(Color::Yellow)
    } else if line.contains("[INFO]") {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };
    vec![Span::styled(line.to_string(), style)]
}
