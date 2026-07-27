use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table},
    Frame,
};

use crate::bam_boost_handler::format_jitosol;
use crate::batch_claim::ClaimState;
use crate::scanner::EpochStatus;
use crate::tui::app::{App, Screen, SetupField};

/// Renders the current screen of the TUI. Pure rendering: no state mutation.
pub fn draw(frame: &mut Frame, app: &App) {
    match app.screen {
        Screen::Setup => draw_setup(frame, app),
        Screen::Dashboard => draw_dashboard(frame, app, frame.area()),
        Screen::Confirm => {
            draw_dashboard(frame, app, frame.area());
            draw_confirm_popup(frame, app);
        }
        Screen::Progress => draw_progress(frame, app),
    }
}

fn setup_field_title(base: &str, field: SetupField, app: &App) -> String {
    if app.setup_focus == field {
        format!("{base} ◀")
    } else {
        base.to_string()
    }
}

fn setup_field_style(field: SetupField, app: &App) -> Style {
    if app.setup_focus == field {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn draw_setup(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let has_error = app.setup_error.is_some();
    let mut constraints = vec![
        Constraint::Length(3), // Network
        Constraint::Length(3), // RPC URL
        Constraint::Length(3), // Claimant
        Constraint::Length(3), // Start
    ];
    if has_error {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1)); // spacer
    constraints.push(Constraint::Length(1)); // footer

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let network_title = setup_field_title("Network", SetupField::Network, app);
    let network = Paragraph::new(app.network.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .title(network_title)
            .border_style(setup_field_style(SetupField::Network, app)),
    );
    frame.render_widget(network, chunks[0]);

    let rpc_title = setup_field_title("RPC URL", SetupField::RpcUrl, app);
    let rpc = Paragraph::new(app.rpc_url.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .title(rpc_title)
            .border_style(setup_field_style(SetupField::RpcUrl, app)),
    );
    frame.render_widget(rpc, chunks[1]);

    let claimant_title = setup_field_title("Claimant", SetupField::Claimant, app);
    let claimant = Paragraph::new(app.claimant_input.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .title(claimant_title)
            .border_style(setup_field_style(SetupField::Claimant, app)),
    );
    frame.render_widget(claimant, chunks[2]);

    let start_title = setup_field_title("[Start scan]", SetupField::Start, app);
    let start = Paragraph::new("").block(
        Block::default()
            .borders(Borders::ALL)
            .title(start_title)
            .border_style(setup_field_style(SetupField::Start, app)),
    );
    frame.render_widget(start, chunks[3]);

    let mut idx = 4;
    if let Some(err) = &app.setup_error {
        let error_para =
            Paragraph::new(err.as_str()).style(Style::default().fg(ratatui::style::Color::Red));
        frame.render_widget(error_para, chunks[idx]);
        idx += 1;
    }
    idx += 1; // skip spacer

    let footer = Paragraph::new("Tab: next · Enter on [Start scan]: scan · Esc: quit");
    frame.render_widget(footer, chunks[idx]);
}

fn status_str(status: &EpochStatus) -> &'static str {
    if status.amount.is_none() {
        "not eligible"
    } else if status.claimed {
        "claimed"
    } else {
        "unclaimed"
    }
}

fn sel_str(app: &App, status: &EpochStatus) -> &'static str {
    if !status.is_claimable() {
        "   "
    } else if app.selected.contains(&status.epoch) {
        "[x]"
    } else {
        "[ ]"
    }
}

fn draw_dashboard(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // table / scanning
            Constraint::Length(1), // footer
        ])
        .split(area);

    let header = Paragraph::new(format!(
        "Claimant: {}    Network: {}",
        app.claimant_input, app.network
    ))
    .block(Block::default().borders(Borders::ALL).title("Dashboard"));
    frame.render_widget(header, chunks[0]);

    if app.scanning {
        let scanning = Paragraph::new("Scanning...");
        frame.render_widget(scanning, chunks[1]);
    } else {
        let rows: Vec<Row> = app
            .statuses
            .iter()
            .enumerate()
            .map(|(i, status)| {
                let amount_str = status
                    .amount
                    .map(format_jitosol)
                    .unwrap_or_else(|| "-".to_string());
                let row = Row::new(vec![
                    sel_str(app, status).to_string(),
                    status.epoch.to_string(),
                    amount_str,
                    status_str(status).to_string(),
                ]);
                if i == app.cursor {
                    row.style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    row
                }
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(5),
                Constraint::Length(10),
                Constraint::Length(20),
                Constraint::Length(15),
            ],
        )
        .header(Row::new(vec!["Sel", "Epoch", "Amount (JitoSOL)", "Status"]));
        frame.render_widget(table, chunks[1]);
    }

    let (unclaimed_count, unclaimed_total) = app
        .statuses
        .iter()
        .filter(|s| s.is_claimable())
        .fold((0usize, 0u64), |(count, total), s| {
            (count + 1, total + s.amount.unwrap_or(0))
        });
    let footer = Paragraph::new(format!(
        "Unclaimed: {} epochs, {} JitoSOL    space select · a all · c claim · r rescan · q quit",
        unclaimed_count,
        format_jitosol(unclaimed_total)
    ));
    frame.render_widget(footer, chunks[2]);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn draw_confirm_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);

    let mut epochs: Vec<u64> = app.selected.iter().copied().collect();
    epochs.sort_unstable();

    let mut lines: Vec<String> = epochs.iter().take(10).map(|e| e.to_string()).collect();
    if epochs.len() > 10 {
        lines.push(format!("… and {} more", epochs.len() - 10));
    }

    let total: u64 = epochs
        .iter()
        .filter_map(|e| {
            app.statuses
                .iter()
                .find(|s| s.epoch == *e)
                .and_then(|s| s.amount)
        })
        .sum();
    lines.push(String::new());
    lines.push(format!("Total: {} JitoSOL", format_jitosol(total)));
    lines.push(format!("Keypair path: {}", app.keypair_input));
    lines.push(String::new());
    lines.push("y confirm · n cancel".to_string());

    let text = lines.join("\n");
    let popup = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Confirm claim"),
        )
        .alignment(Alignment::Left);
    frame.render_widget(popup, area);
}

fn claim_state_str(state: &ClaimState) -> String {
    match state {
        ClaimState::Started => "…".to_string(),
        ClaimState::Success(sig) => format!("OK {sig}"),
        ClaimState::Failed(err) => format!("FAIL {err}"),
        ClaimState::Skipped(reason) => format!("skip {reason}"),
    }
}

fn draw_progress(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let rows: Vec<Row> = app
        .progress_rows
        .iter()
        .map(|ev| Row::new(vec![ev.epoch.to_string(), claim_state_str(&ev.state)]))
        .collect();

    let table = Table::new(rows, [Constraint::Length(10), Constraint::Min(1)])
        .header(Row::new(vec!["Epoch", "State"]))
        .block(Block::default().borders(Borders::ALL).title("Progress"));
    frame.render_widget(table, chunks[0]);

    if app.claim_done {
        let footer = Paragraph::new("b back to dashboard · q quit");
        frame.render_widget(footer, chunks[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::EpochStatus;
    use crate::tui::app::App;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn dashboard_shows_epochs_and_unclaimed_summary() {
        let mut app = App::new();
        app.screen = crate::tui::app::Screen::Dashboard;
        app.claimant_input = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn".into();
        app.statuses = vec![
            EpochStatus {
                epoch: 1000,
                amount: Some(1_500_000_000),
                claimed: false,
            },
            EpochStatus {
                epoch: 999,
                amount: Some(1_000_000_000),
                claimed: true,
            },
            EpochStatus {
                epoch: 998,
                amount: None,
                claimed: false,
            },
        ];

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("1000"));
        assert!(text.contains("unclaimed"));
        assert!(text.contains("claimed"));
        assert!(text.contains("not eligible"));
        assert!(text.contains("1.500000000")); // formatted JitoSOL
    }

    #[test]
    fn setup_screen_shows_fields() {
        let app = App::new();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Network"));
        assert!(text.contains("RPC URL"));
        assert!(text.contains("Claimant"));
    }
}
