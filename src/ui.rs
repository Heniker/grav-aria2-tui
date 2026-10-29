use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, Wrap};

use crate::app::{
    App, Modal, TorrentStatus, format_bytes, format_eta, format_speed,
};


pub fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // Header + Tag Filter Bar
            Constraint::Min(6),    // Main Table
            Constraint::Length(5), // Detail & Help Bar
        ])
        .split(f.area());

    render_header(f, app, chunks[0]);
    render_table(f, app, chunks[1]);
    render_footer(f, app, chunks[2]);

    render_modal(f, app);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let header_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(2)])
        .split(area);

    // Top row: App title + directory + RPC badge | Speeds and limits
    let rpc_badge = if app.spawned_by_us {
        Span::styled(" [Connected: Spawned] ", Style::default().fg(Color::Green))
    } else {
        Span::styled(" [Connected: External] ", Style::default().fg(Color::Cyan))
    };

    let dl_spd = app.global_stat.download_speed.parse::<u64>().unwrap_or(0);
    let ul_spd = app.global_stat.upload_speed.parse::<u64>().unwrap_or(0);

    let left_spans = vec![
        Span::styled(
            " GRAV-ARIA2 ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("dir: {}", app.dir.display()),
            Style::default().fg(Color::Yellow),
        ),
        rpc_badge,
    ];

    let right_spans = vec![
        Span::styled("DL: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_speed(dl_spd),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" (lim: {})", app.global_dl_limit),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
        Span::styled("UL: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_speed(ul_spd),
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" (lim: {})", app.global_ul_limit),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
    ];

    let row_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(header_chunks[0]);

    f.render_widget(Paragraph::new(Line::from(left_spans)), row_layout[0]);
    f.render_widget(
        Paragraph::new(Line::from(right_spans)).alignment(Alignment::Right),
        row_layout[1],
    );

    // Bottom row: Tag Filter Bar
    let tags = app.available_tags();
    let mut tag_spans = vec![Span::styled(
        " Tags: ",
        Style::default().fg(Color::DarkGray),
    )];

    for (i, tag) in tags.iter().enumerate() {
        let is_selected = i == app.tag_filter_index;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        tag_spans.push(Span::styled(format!(" [{}] ", tag), style));
        tag_spans.push(Span::raw(" "));
    }

    f.render_widget(Paragraph::new(Line::from(tag_spans)), header_chunks[1]);
}

fn render_table(f: &mut Frame, app: &App, area: Rect) {
    let filtered_items = app.filtered_items();

    let header = Row::new(vec![
        "Name",
        "Tags",
        "Size",
        "Progress",
        "Status",
        "DL Speed",
        "UL Speed",
        "Ratio",
        "ETA",
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(0);

    let rows: Vec<Row> = filtered_items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let is_selected = idx == app.selected_index;

            let (status_text, status_style) = match item.status {
                TorrentStatus::Downloading => (
                    "Downloading",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                TorrentStatus::Seeding => (
                    "Seeding",
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::BOLD),
                ),
                TorrentStatus::Paused => ("Paused", Style::default().fg(Color::Yellow)),
                TorrentStatus::FetchingMetadata => (
                    "Fetching Metadata...",
                    Style::default().fg(Color::Magenta),
                ),
                TorrentStatus::SelectFiles => (
                    "Select Files (f)",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                TorrentStatus::StoppedSaved => {
                    ("Stopped (Saved)", Style::default().fg(Color::DarkGray))
                }
                TorrentStatus::Error => {
                    ("Error", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                }
            };

            let progress_str = if item.status == TorrentStatus::FetchingMetadata {
                "-".to_string()
            } else {
                format!("{:.1}%", item.progress_pct())
            };

            let tags_str = if item.tags.is_empty() {
                "-".to_string()
            } else {
                item.tags.join(",")
            };

            let size_str = if item.size_bytes == 0 {
                "-".to_string()
            } else {
                format_bytes(item.size_bytes)
            };

            let dl_spd_str = format_speed(item.download_speed);
            let ul_spd_str = format_speed(item.upload_speed);
            let ratio_str = format!("{:.2}", item.ratio());
            let eta_str = format_eta(item.eta_seconds());

            let row = Row::new(vec![
                Span::raw(truncate_str(&item.name, 45)),
                Span::raw(truncate_str(&tags_str, 15)),
                Span::raw(size_str),
                Span::raw(progress_str),
                Span::styled(status_text, status_style),
                Span::raw(dl_spd_str),
                Span::raw(ul_spd_str),
                Span::raw(ratio_str),
                Span::raw(eta_str),
            ]);

            if is_selected {
                row.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                row
            }
        })
        .collect();

    let widths = [
        Constraint::Percentage(32), // Name
        Constraint::Length(12),     // Tags
        Constraint::Length(10),     // Size
        Constraint::Length(9),      // Progress
        Constraint::Length(18),     // Status
        Constraint::Length(11),     // DL Speed
        Constraint::Length(11),     // UL Speed
        Constraint::Length(7),      // Ratio
        Constraint::Length(9),      // ETA
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" BitTorrent Tasks ")
                .border_style(Style::default().fg(Color::Blue)),
        );

    f.render_widget(table, area);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Details & Controls ")
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let sel = app.selected_torrent();
    let mut lines = Vec::new();

    if let Some(item) = sel {
        // Line 1: InfoHash and Name
        let hash_span = Span::styled(&item.info_hash, Style::default().fg(Color::Yellow));
        let sel_files_info = if item.selected_files.is_empty() {
            "All".to_string()
        } else {
            format!("Indices [{}]", item.selected_files)
        };

        lines.push(Line::from(vec![
            Span::styled("Hash: ", Style::default().fg(Color::DarkGray)),
            hash_span,
            Span::raw(" | "),
            Span::styled("Files: ", Style::default().fg(Color::DarkGray)),
            Span::styled(sel_files_info, Style::default().fg(Color::White)),
            Span::raw(" | "),
            Span::styled("Limits: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("DL {} / UL {}", item.max_download_limit, item.max_upload_limit)),
            Span::raw(" | "),
            Span::styled("Seed: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}x / {}m", item.seed_ratio, item.seed_time)),
        ]));

        if let Some(ref err) = item.error_message {
            lines.push(Line::from(vec![
                Span::styled("Error: ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled(err, Style::default().fg(Color::Red)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("Peers: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{} seeders", item.num_seeders.as_deref().unwrap_or("0"))),
                Span::raw(" | "),
                Span::styled("Connections: ", Style::default().fg(Color::DarkGray)),
                Span::raw(item.connections.as_deref().unwrap_or("0")),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No torrent selected. Press 'a' to add a torrent.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::default());
    }

    // Legend line
    lines.push(Line::from(vec![
        Span::styled("[j/k]:", Style::default().fg(Color::Yellow)),
        Span::raw("Nav "),
        Span::styled("[Tab]:", Style::default().fg(Color::Yellow)),
        Span::raw("Tag "),
        Span::styled("[s/Space]:", Style::default().fg(Color::Yellow)),
        Span::raw("Toggle "),
        Span::styled("[S/P]:", Style::default().fg(Color::Yellow)),
        Span::raw("Batch "),
        Span::styled("[a]:", Style::default().fg(Color::Yellow)),
        Span::raw("Add "),
        Span::styled("[f]:", Style::default().fg(Color::Yellow)),
        Span::raw("Files "),
        Span::styled("[t]:", Style::default().fg(Color::Yellow)),
        Span::raw("Tags "),
        Span::styled("[o]:", Style::default().fg(Color::Yellow)),
        Span::raw("Opts "),
        Span::styled("[g]:", Style::default().fg(Color::Yellow)),
        Span::raw("Global "),
        Span::styled("[v]:", Style::default().fg(Color::Yellow)),
        Span::raw("Verify "),
        Span::styled("[d]:", Style::default().fg(Color::Yellow)),
        Span::raw("Del "),
        Span::styled("[q]:", Style::default().fg(Color::Yellow)),
        Span::raw("Quit"),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

fn render_modal(f: &mut Frame, app: &App) {
    match &app.modal {
        Modal::None => {}
        Modal::AddTorrent {
            path_or_magnet,
            tags,
            active_field,
            ..
        } => {
            let area = centered_rect(65, 30, f.area());
            f.render_widget(Clear, area);

            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Add BitTorrent (.torrent or magnet:) ")
                .border_style(Style::default().fg(Color::Cyan));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2), // Path/Magnet label + input
                    Constraint::Length(2), // Tags label + input
                    Constraint::Length(1), // Help legend
                ])
                .split(inner);

            let f0_style = if *active_field == 0 {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let f1_style = if *active_field == 1 {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let p0 = Paragraph::new(vec![
                Line::from(Span::styled("File path or magnet: URI:", f0_style)),
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled(path_or_magnet, Style::default().fg(Color::Cyan)),
                    if *active_field == 0 {
                        Span::styled("█", Style::default().fg(Color::Yellow))
                    } else {
                        Span::raw("")
                    },
                ]),
            ]);
            f.render_widget(p0, chunks[0]);

            let p1 = Paragraph::new(vec![
                Line::from(Span::styled("Initial tags (comma-separated, optional):", f1_style)),
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled(tags, Style::default().fg(Color::Cyan)),
                    if *active_field == 1 {
                        Span::styled("█", Style::default().fg(Color::Yellow))
                    } else {
                        Span::raw("")
                    },
                ]),
            ]);
            f.render_widget(p1, chunks[1]);

            let legend = Paragraph::new(Line::from(vec![
                Span::styled("[Tab/Down/Up]", Style::default().fg(Color::Yellow)),
                Span::raw(" Switch  "),
                Span::styled("[Enter]", Style::default().fg(Color::Green)),
                Span::raw(" Add  "),
                Span::styled("[Esc]", Style::default().fg(Color::Red)),
                Span::raw(" Cancel"),
            ]));
            f.render_widget(legend, chunks[2]);
        }
        Modal::FileSelection {
            files,
            selected_index,
            ..
        } => {
            let area = centered_rect(80, 75, f.area());
            f.render_widget(Clear, area);

            let block = Block::default()
                .borders(Borders::ALL)
                .title(" File Selection Modal (Choose files to download) ")
                .border_style(Style::default().fg(Color::Yellow));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(5), Constraint::Length(2)])
                .split(inner);

            // Render scrollable file list
            let total_files = files.len();
            let visible_height = chunks[0].height as usize;
            let start_idx = if *selected_index >= visible_height {
                *selected_index - visible_height + 1
            } else {
                0
            };
            let end_idx = (start_idx + visible_height).min(total_files);

            let mut lines = Vec::new();
            for i in start_idx..end_idx {
                let file = &files[i];
                let is_cur = i == *selected_index;
                let check = if file.selected { "[x]" } else { "[ ]" };
                let size_str = format_bytes(file.length);

                let line_str = format!(
                    "{} {:>3}. {:<10} {}",
                    check, file.index, size_str, file.path
                );

                if is_cur {
                    lines.push(Line::from(Span::styled(
                        line_str,
                        Style::default()
                            .bg(Color::DarkGray)
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )));
                } else {
                    let color = if file.selected {
                        Color::White
                    } else {
                        Color::DarkGray
                    };
                    lines.push(Line::from(Span::styled(line_str, Style::default().fg(color))));
                }
            }

            f.render_widget(Paragraph::new(lines), chunks[0]);

            let legend = Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("[j/k / Up/Down]", Style::default().fg(Color::Yellow)),
                    Span::raw(" Move  "),
                    Span::styled("[Space]", Style::default().fg(Color::Cyan)),
                    Span::raw(" Toggle  "),
                    Span::styled("[a]", Style::default().fg(Color::Cyan)),
                    Span::raw(" All  "),
                    Span::styled("[n]", Style::default().fg(Color::Cyan)),
                    Span::raw(" None  "),
                    Span::styled("[Enter]", Style::default().fg(Color::Green)),
                    Span::raw(" Confirm & Start  "),
                    Span::styled("[Esc]", Style::default().fg(Color::Red)),
                    Span::raw(" Cancel"),
                ]),
            ]);
            f.render_widget(legend, chunks[1]);
        }
        Modal::EditTags { tags, .. } => {
            let area = centered_rect(55, 20, f.area());
            f.render_widget(Clear, area);

            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Edit Torrent Tags ")
                .border_style(Style::default().fg(Color::Cyan));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(2), Constraint::Length(1)])
                .split(inner);

            let input_lines = vec![
                Line::from("Enter comma-separated tags (e.g. linux, iso, arch):"),
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled(tags, Style::default().fg(Color::Yellow)),
                    Span::styled("█", Style::default().fg(Color::Cyan)),
                ]),
            ];
            f.render_widget(Paragraph::new(input_lines), chunks[0]);

            let legend = Paragraph::new(Line::from(vec![
                Span::styled("[Enter]", Style::default().fg(Color::Green)),
                Span::raw(" Save  "),
                Span::styled("[Esc]", Style::default().fg(Color::Red)),
                Span::raw(" Cancel"),
            ]));
            f.render_widget(legend, chunks[1]);
        }
        Modal::TorrentOptions {
            max_download_limit,
            max_upload_limit,
            seed_ratio,
            seed_time,
            active_field,
            ..
        } => {
            let area = centered_rect(65, 45, f.area());
            f.render_widget(Clear, area);

            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Per-Torrent Options ")
                .border_style(Style::default().fg(Color::Cyan));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Length(2),
                    Constraint::Length(2),
                    Constraint::Length(2),
                    Constraint::Length(1),
                ])
                .split(inner);

            let fields = [
                ("Max Download Limit (0 = unlim, e.g. 500K, 5M):", max_download_limit),
                ("Max Upload Limit (0 = unlim, e.g. 200K, 2M):", max_upload_limit),
                ("Seed Ratio Target (e.g. 1.0, 0.0 = unlimited):", seed_ratio),
                ("Seed Time Target in minutes (0 = unlim, e.g. 60):", seed_time),
            ];

            for (i, (label, val)) in fields.iter().enumerate() {
                let is_active = i == *active_field;
                let lbl_style = if is_active {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let p = Paragraph::new(vec![
                    Line::from(Span::styled(*label, lbl_style)),
                    Line::from(vec![
                        Span::raw("> "),
                        Span::styled(*val, Style::default().fg(Color::Cyan)),
                        if is_active {
                            Span::styled("█", Style::default().fg(Color::Yellow))
                        } else {
                            Span::raw("")
                        },
                    ]),
                ]);
                f.render_widget(p, chunks[i]);
            }

            let legend = Paragraph::new(Line::from(vec![
                Span::styled("[Tab/Down/Up]", Style::default().fg(Color::Yellow)),
                Span::raw(" Switch  "),
                Span::styled("[Enter]", Style::default().fg(Color::Green)),
                Span::raw(" Save  "),
                Span::styled("[Esc]", Style::default().fg(Color::Red)),
                Span::raw(" Cancel"),
            ]));
            f.render_widget(legend, chunks[4]);
        }
        Modal::GlobalOptions {
            max_overall_download_limit,
            max_overall_upload_limit,
            default_seed_ratio,
            default_seed_time,
            active_field,
            ..
        } => {
            let area = centered_rect(65, 45, f.area());
            f.render_widget(Clear, area);

            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Global Limits & Seeding Options ")
                .border_style(Style::default().fg(Color::Cyan));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Length(2),
                    Constraint::Length(2),
                    Constraint::Length(2),
                    Constraint::Length(1),
                ])
                .split(inner);

            let fields = [
                ("Max Overall Download Limit (0 = unlim, e.g. 10M):", max_overall_download_limit),
                ("Max Overall Upload Limit (0 = unlim, e.g. 5M):", max_overall_upload_limit),
                ("Default Seed Ratio (e.g. 1.0):", default_seed_ratio),
                ("Default Seed Time in minutes (0 = unlim, e.g. 60):", default_seed_time),
            ];

            for (i, (label, val)) in fields.iter().enumerate() {
                let is_active = i == *active_field;
                let lbl_style = if is_active {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let p = Paragraph::new(vec![
                    Line::from(Span::styled(*label, lbl_style)),
                    Line::from(vec![
                        Span::raw("> "),
                        Span::styled(*val, Style::default().fg(Color::Cyan)),
                        if is_active {
                            Span::styled("█", Style::default().fg(Color::Yellow))
                        } else {
                            Span::raw("")
                        },
                    ]),
                ]);
                f.render_widget(p, chunks[i]);
            }

            let legend = Paragraph::new(Line::from(vec![
                Span::styled("[Tab/Down/Up]", Style::default().fg(Color::Yellow)),
                Span::raw(" Switch  "),
                Span::styled("[Enter]", Style::default().fg(Color::Green)),
                Span::raw(" Save  "),
                Span::styled("[Esc]", Style::default().fg(Color::Red)),
                Span::raw(" Cancel"),
            ]));
            f.render_widget(legend, chunks[4]);
        }
        Modal::DeleteConfirm {
            name,
            delete_files_from_disk,
            ..
        } => {
            let area = centered_rect(65, 30, f.area());
            f.render_widget(Clear, area);

            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Remove Torrent Confirmation ")
                .border_style(Style::default().fg(Color::Red));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(inner);

            let prompt = Paragraph::new(vec![
                Line::from(format!("Are you sure you want to remove: '{}'?", name)),
            ]);
            f.render_widget(prompt, chunks[0]);

            let opt1_style = if !*delete_files_from_disk {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let opt2_style = if *delete_files_from_disk {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let options = Paragraph::new(vec![
                Line::from(Span::styled(
                    " [1] Remove from TUI and .torrent/ ONLY (Keep payload files on disk)",
                    opt1_style,
                )),
                Line::from(Span::styled(
                    " [2] Remove from TUI, .torrent/, AND DELETE payload files from disk",
                    opt2_style,
                )),
            ]);
            f.render_widget(options, chunks[1]);

            let legend = Paragraph::new(Line::from(vec![
                Span::styled("[1/2 or Up/Down]", Style::default().fg(Color::Yellow)),
                Span::raw(" Select  "),
                Span::styled("[Enter]", Style::default().fg(Color::Green)),
                Span::raw(" Confirm  "),
                Span::styled("[Esc]", Style::default().fg(Color::Cyan)),
                Span::raw(" Cancel"),
            ]));
            f.render_widget(legend, chunks[2]);
        }
        Modal::Message {
            title,
            message,
            is_error,
        } => {
            let area = centered_rect(55, 25, f.area());
            f.render_widget(Clear, area);

            let border_color = if *is_error { Color::Red } else { Color::Cyan };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", title))
                .border_style(Style::default().fg(border_color));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(2), Constraint::Length(1)])
                .split(inner);

            let msg_paragraph = Paragraph::new(message.as_str())
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::White));
            f.render_widget(msg_paragraph, chunks[0]);

            let legend = Paragraph::new(Line::from(vec![
                Span::styled("[Enter / Esc]", Style::default().fg(Color::Yellow)),
                Span::raw(" Close"),
            ]));
            f.render_widget(legend, chunks[1]);
        }
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
