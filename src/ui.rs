use crate::types::{
    AdjustDirection, EventBuffer, PacketEvent, ProxySettings, SettingKind, format_rate, next_rate,
    prev_rate,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use parking_lot::RwLock;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};

struct UiState {
    selected: SettingKind,
    events: Vec<PacketEvent>,
}

pub fn run_ui(
    in_addr: SocketAddr,
    out_addr: SocketAddr,
    refresh: Duration,
    settings: Arc<RwLock<ProxySettings>>,
    events: EventBuffer,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = ui_loop(&mut terminal, in_addr, out_addr, refresh, settings, events);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn ui_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    in_addr: SocketAddr,
    out_addr: SocketAddr,
    refresh: Duration,
    settings: Arc<RwLock<ProxySettings>>,
    events: EventBuffer,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ui = UiState {
        selected: SettingKind::Latency,
        events: Vec::new(),
    };

    loop {
        let size = terminal.size()?;
        let table_height = size.height.saturating_sub(6);
        let max_rows = table_height.saturating_sub(3).max(1) as usize;
        ui.events = {
            let guard = events.lock();
            guard.iter().take(max_rows).cloned().collect()
        };

        let settings_snapshot = settings.read().clone();
        terminal.draw(|frame| {
            draw_ui(frame, in_addr, out_addr, refresh, &settings_snapshot, &ui);
        })?;

        if event::poll(refresh)?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if handle_key(key.code, &settings, &mut ui) {
                break;
            }
        }
    }

    Ok(())
}

fn handle_key(code: KeyCode, settings: &Arc<RwLock<ProxySettings>>, ui: &mut UiState) -> bool {
    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('l') => {
            let mut settings = settings.write();
            settings.latency_enabled = !settings.latency_enabled;
        }
        KeyCode::Char('j') => {
            let mut settings = settings.write();
            settings.jitter_enabled = !settings.jitter_enabled;
        }
        KeyCode::Char('p') => {
            let mut settings = settings.write();
            if settings.loss_enabled {
                settings.loss_enabled = false;
            } else {
                if settings.loss_percent <= 0.0 {
                    settings.loss_percent = 1.0;
                }
                settings.loss_enabled = true;
            }
        }
        KeyCode::Char('r') => {
            let mut settings = settings.write();
            if settings.rate_enabled {
                settings.rate_enabled = false;
            } else {
                if settings.rate_bytes_per_sec == 0 {
                    settings.rate_bytes_per_sec = next_rate(0);
                }
                settings.rate_enabled = true;
            }
        }
        KeyCode::Tab | KeyCode::Right => ui.selected = ui.selected.next(),
        KeyCode::BackTab | KeyCode::Left => ui.selected = ui.selected.prev(),
        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Up => {
            adjust_setting(settings, ui.selected, AdjustDirection::Increase);
        }
        KeyCode::Char('-') | KeyCode::Down => {
            adjust_setting(settings, ui.selected, AdjustDirection::Decrease);
        }
        _ => {}
    }

    false
}

fn adjust_setting(
    settings: &Arc<RwLock<ProxySettings>>,
    kind: SettingKind,
    direction: AdjustDirection,
) {
    let mut settings = settings.write();
    match kind {
        SettingKind::Latency => {
            let step = 10u64;
            if matches!(direction, AdjustDirection::Increase) {
                settings.latency_ms = settings.latency_ms.saturating_add(step);
            } else {
                settings.latency_ms = settings.latency_ms.saturating_sub(step);
            }
            settings.latency_enabled = settings.latency_ms > 0;
        }
        SettingKind::Jitter => {
            let step = 10u64;
            if matches!(direction, AdjustDirection::Increase) {
                settings.jitter_ms = settings.jitter_ms.saturating_add(step);
            } else {
                settings.jitter_ms = settings.jitter_ms.saturating_sub(step);
            }
            settings.jitter_enabled = settings.jitter_ms > 0;
        }
        SettingKind::Loss => {
            let step = 1.0f64;
            let delta = if matches!(direction, AdjustDirection::Increase) {
                step
            } else {
                -step
            };
            let next = settings.loss_percent + delta;
            settings.loss_percent = next.clamp(0.0, 100.0);
            settings.loss_enabled = settings.loss_percent > 0.0;
        }
        SettingKind::Rate => {
            settings.rate_bytes_per_sec = match direction {
                AdjustDirection::Increase => next_rate(settings.rate_bytes_per_sec),
                AdjustDirection::Decrease => prev_rate(settings.rate_bytes_per_sec),
            };
            settings.rate_enabled = settings.rate_bytes_per_sec > 0;
        }
    }
}

fn draw_ui(
    frame: &mut Frame,
    in_addr: SocketAddr,
    out_addr: SocketAddr,
    refresh: Duration,
    settings: &ProxySettings,
    ui: &UiState,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(5)])
        .split(frame.area());

    let header = build_header(
        in_addr,
        out_addr,
        refresh,
        settings,
        ui.selected,
        frame.area().width,
    );
    frame.render_widget(header, chunks[0]);

    let table = build_table(ui);
    frame.render_widget(table, chunks[1]);
}

fn build_header(
    in_addr: SocketAddr,
    out_addr: SocketAddr,
    refresh: Duration,
    settings: &ProxySettings,
    selected: SettingKind,
    width: u16,
) -> Paragraph<'static> {
    let title_style = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    let line1 = Line::from(vec![
        Span::styled("udptoxy", title_style),
        Span::raw("  In: "),
        Span::raw(in_addr.to_string()),
        Span::raw("  Out: "),
        Span::raw(out_addr.to_string()),
        Span::raw("  Refresh: "),
        Span::raw(format!("{}ms", refresh.as_millis())),
    ]);

    let latency_style = setting_style(settings.latency_enabled, selected == SettingKind::Latency);
    let jitter_style = setting_style(settings.jitter_enabled, selected == SettingKind::Jitter);
    let loss_style = setting_style(settings.loss_enabled, selected == SettingKind::Loss);
    let rate_style = setting_style(settings.rate_enabled, selected == SettingKind::Rate);

    let latency_text = format!(
        "Latency [l] {} {}ms",
        on_off(settings.latency_enabled),
        settings.latency_ms
    );
    let jitter_text = format!(
        "Jitter [j] {} {}ms",
        on_off(settings.jitter_enabled),
        settings.jitter_ms
    );
    let loss_text = format!(
        "Loss [p] {} {:.1}%",
        on_off(settings.loss_enabled),
        settings.loss_percent
    );
    let rate_text = format!(
        "Rate [r] {} {}",
        on_off(settings.rate_enabled),
        format_rate(settings.rate_bytes_per_sec)
    );
    let sep = "  |  ";
    let inner_width = width.saturating_sub(2) as usize;
    let combined_len = latency_text.len()
        + sep.len()
        + jitter_text.len()
        + sep.len()
        + loss_text.len()
        + sep.len()
        + rate_text.len();
    let split = inner_width > 0 && combined_len > inner_width;

    let mut settings_lines = Vec::new();
    if split {
        settings_lines.push(Line::from(vec![
            Span::styled(latency_text, latency_style),
            Span::styled(sep, dim),
            Span::styled(jitter_text, jitter_style),
        ]));
        settings_lines.push(Line::from(vec![
            Span::styled(loss_text, loss_style),
            Span::styled(sep, dim),
            Span::styled(rate_text, rate_style),
        ]));
    } else {
        settings_lines.push(Line::from(vec![
            Span::styled(latency_text, latency_style),
            Span::styled(sep, dim),
            Span::styled(jitter_text, jitter_style),
            Span::styled(sep, dim),
            Span::styled(loss_text, loss_style),
            Span::styled(sep, dim),
            Span::styled(rate_text, rate_style),
        ]));
    }

    let line3 = Line::from(vec![
        Span::raw("Select [Tab or Left/Right]  "),
        Span::raw("Adjust [+/- or Up/Down]  "),
        Span::raw("Quit [q]"),
    ]);

    let mut lines = Vec::new();
    lines.push(line1);
    lines.extend(settings_lines);
    lines.push(line3);

    Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Status"))
}

fn build_table(ui: &UiState) -> Table<'static> {
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("dir"),
        Cell::from("in(ms)"),
        Cell::from("lat(ms)"),
        Cell::from("out(ms)"),
        Cell::from("action"),
        Cell::from("bytes"),
        Cell::from("src"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows = ui.events.iter().map(|event| {
        let out = event
            .time_out_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        Row::new(vec![
            Cell::from(event.id.to_string()),
            Cell::from(event.direction.as_str()),
            Cell::from(event.time_in_ms.to_string()),
            Cell::from(event.latency_ms.to_string()),
            Cell::from(out),
            Cell::from(event.action.as_str()),
            Cell::from(event.size.to_string()),
            Cell::from(event.src.to_string()),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Min(15),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Packets"))
}

fn setting_style(enabled: bool, selected: bool) -> Style {
    let mut style = if enabled {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn on_off(enabled: bool) -> &'static str {
    if enabled { "ON" } else { "OFF" }
}
