    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::{
        Shell, ShellAction, View, capture_rendered_buffer, capture_rendered_cell_colors, theme,
    };

    #[test]
    fn routes_the_six_primary_views_by_number_and_keeps_letter_aliases() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));

        for (key, expected_view) in [
            ('2', View::Trains),
            ('4', View::Company),
            ('3', View::BuyTrains),
            ('1', View::Map),
            ('5', View::Authority),
            ('6', View::Bulletin),
            ('t', View::Trains),
            ('c', View::Company),
            ('b', View::BuyTrains),
            ('m', View::Map),
            ('a', View::Authority),
            ('u', View::Bulletin),
        ] {
            assert_eq!(
                shell.handle_key(
                    KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                    &state
                ),
                ShellAction::Continue
            );
            assert_eq!(shell.active_view(), expected_view);
        }
    }

    #[test]
    fn map_world_details_explains_the_region_registration_identity() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
                &state,
            ),
            ShellAction::Continue
        );
        assert!(shell.map_workspace.world_details_visible());

        let rendered = capture_rendered_buffer(&shell, &state, 120, 40);
        let registration = format!(
            "{} · {}",
            state.region.railway_registration.display_code(),
            state.region.railway_registration.mark
        );
        assert!(rendered.contains("World Details"));
        assert!(rendered.contains(&state.region.name));
        assert!(rendered.contains(&state.region.rail_authority.name));
        assert!(rendered.contains(&registration));
        assert!(rendered.contains("RAILWAY REGISTRATION"));
        assert!(rendered.contains("fictional two-letter railway mark"));
        assert!(rendered.contains("Connected"));
        assert!(rendered.contains("Rail network"));
        assert!(rendered.contains("[Esc/W] close"));
        assert!(!rendered.contains("w / Esc · return to Map"));
        assert_eq!(
            capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
            Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
            "World Details should mute the application underneath it",
        );
        assert_eq!(
            capture_rendered_cell_colors(&shell, &state, 120, 40, 17, 8),
            Some((theme::ACCENT, theme::PANEL)),
            "World Details should use the shared focused-modal border",
        );

        assert_eq!(
            shell.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state),
            ShellAction::Continue
        );
        assert!(!shell.map_workspace.world_details_visible());
    }

    #[test]
    fn company_vkm_editor_normalizes_input_and_emits_a_persisted_action() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE),
                &state,
            ),
            ShellAction::Continue
        );
        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
                &state,
            ),
            ShellAction::Continue
        );
        assert!(shell.company_workspace.has_vkm_editor());
        let rendered = capture_rendered_buffer(&shell, &state, 120, 40);
        assert!(rendered.contains("Edit Vehicle Keeper Mark"));
        assert!(rendered.contains("VEHICLE KEEPER MARK"));
        assert!(rendered.contains("[Enter] save"));
        assert!(rendered.contains("[Backspace] delete"));
        assert!(rendered.contains("[Esc] cancel"));
        assert_eq!(
            capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
            Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
            "VKM editor should mute the Company dashboard underneath it",
        );

        for _ in 0..3 {
            assert_eq!(
                shell.handle_key(
                    KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                    &state,
                ),
                ShellAction::Continue
            );
        }
        for character in ['o', 'm', 'p'] {
            assert_eq!(
                shell.handle_key(
                    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                    &state,
                ),
                ShellAction::Continue
            );
        }

        let action = shell.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state);
        match action {
            ShellAction::Player(AppCommand::UpdateCompanyVkm {
                vehicle_keeper_mark,
            }) => {
                assert_eq!(vehicle_keeper_mark.as_str(), "OMP");
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn routes_quit_and_control_c_to_clean_exit() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Exit
        );
        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &state
            ),
            ShellAction::Exit
        );
    }

    #[test]
    fn small_terminal_gets_a_resize_hint() {
        assert!(Shell::new().resize_hint(63, 16).is_some());
        assert!(Shell::new().resize_hint(64, 15).is_some());
        assert!(Shell::new().resize_hint(64, 16).is_none());
    }

    #[test]
    fn narrow_terminal_frame_renders_resize_hint_without_panicking() {
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut shell = Shell::new();
        let state = create_new_game(42, "Narrow Passenger", UtcSeconds::from_unix_seconds(1_000));

        terminal
            .draw(|frame| super::render_frame(frame, &mut shell, &state))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Terminal too small"));
        assert!(rendered.contains("RailQ"));
    }

    #[test]
    fn control_room_shell_reports_company_status_with_adaptive_tabs_and_semantic_surfaces() {
        let started_at = UtcSeconds::from_unix_seconds(1_000);
        let mut state = create_new_game(42, "Northstar Passenger", started_at);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
                .unwrap();
        dispatch_journey(&mut state, train_id, service_id, started_at).unwrap();
        let mut shell = Shell::new();
        let funds = super::format_money(state.player_company.funds);

        let wide = capture_rendered_buffer(&shell, &state, 120, 40);
        assert!(wide.contains("RailQ · Northstar Passenger"));
        assert!(wide.contains(&format!("Company Funds {funds}")));
        assert!(wide.contains("READY 0"));
        assert!(wide.contains("TRAVELLING 1"));
        assert!(wide.contains("ETA"));
        assert!(wide.contains("1 Map"));
        assert!(wide.contains("2 Trains"));
        assert!(wide.contains("q Quit"));

        let compact = capture_rendered_buffer(&shell, &state, 80, 24);
        assert!(compact.contains(&format!("Funds {funds}")));
        assert!(compact.contains("R 0"));
        assert!(compact.contains("T 1"));
        assert!(compact.contains("ETA"));
        assert!(compact.contains("4 Co"));
        assert!(compact.contains("3 Mkt"));
        assert!(compact.contains("q Quit"));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::render_frame(frame, &mut shell, &state))
            .unwrap();
        let cells = terminal.backend().buffer().content();
        assert!(cells.iter().any(|cell| cell.bg == theme::BACKGROUND));
        assert!(cells.iter().any(|cell| cell.bg == theme::PANEL));
        assert!(cells.iter().any(|cell| cell.fg == theme::ACCENT));

        let feedback_state = create_new_game(42, "Feedback Passenger", started_at);
        let mut feedback_shell = Shell::new();
        feedback_shell.handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &feedback_state,
        );
        let feedback = capture_rendered_buffer(&feedback_shell, &feedback_state, 120, 40);
        let feedback_lines = feedback.lines().collect::<Vec<_>>();
        assert!(feedback_lines[38].contains("No READY Train"));
        assert!(feedback_lines[39].contains("q Quit"));

        let feedback_backend = TestBackend::new(120, 40);
        let mut feedback_terminal = Terminal::new(feedback_backend).unwrap();
        feedback_terminal
            .draw(|frame| super::render_frame(frame, &mut feedback_shell, &feedback_state))
            .unwrap();
        let feedback_cell = &feedback_terminal.backend().buffer().content()[120 * 38];
        assert_eq!(feedback_cell.symbol(), "N");
        assert_eq!(feedback_cell.fg, theme::WARNING);
        assert_eq!(feedback_cell.bg, theme::BACKGROUND);
    }

    #[test]
    fn fleet_table_marks_the_selected_train_with_the_accent_surface() {
        let started_at = UtcSeconds::from_unix_seconds(1_000);
        let mut state = create_new_game(42, "Fleet Selection", started_at);
        state.player_company.funds = Money::from_cents(1_000_000);
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();
        shell.handle_key(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
            &state,
        );
        shell.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &state);

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::render_frame(frame, &mut shell, &state))
            .unwrap();
        let marker = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .find(|cell| cell.symbol() == ">")
            .unwrap();
        assert_eq!(marker.fg, theme::BACKGROUND);
        assert_eq!(marker.bg, theme::ACCENT);

        let rendered = capture_rendered_buffer(&shell, &state, 120, 40);
        assert!(rendered.contains("Trains · 2 total · 2 READY · 0 TRAVELLING"));
        assert!(rendered.contains("OPERATIONS"));
        assert!(rendered.contains("SPECIFICATIONS"));
        assert!(rendered.contains("D Dispatch"));
        assert!(!rendered.contains("Enter Inspect"));
    }

    #[test]
    fn map_routes_a_keyboard_manual_dispatch_to_the_application_boundary() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(3))
                .unwrap();
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('d')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::Player(AppCommand::ManualDispatch {
                train_id,
                service_id,
            })
        );
    }

    #[test]
    fn buy_trains_routes_only_an_explicit_confirmation_to_the_application_boundary() {
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('b')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Down), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::Player(AppCommand::PurchaseTrain {
                catalogue_index: 1,
                delivery_station_id: RailStationId::new(1),
            })
        );
    }

    #[test]
    fn fleet_resale_routes_only_a_confirmed_ready_train_to_the_application_boundary() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('t')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Char('s')), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::Player(AppCommand::SellTrain { train_id })
        );
    }

    #[test]
    fn help_is_contextual_and_points_a_new_company_to_the_market() {
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let mut shell = Shell::new();

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert!(shell.help_visible());
        let help = super::help_lines(&shell, &state).join("\n");
        for instruction in [
            "1 Map",
            "2 Trains",
            "3 Market",
            "4 Company",
            "Current · Map",
            "3 Open Market and acquire your first passenger Train",
        ] {
            assert!(help.contains(instruction));
        }
        shell.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state);
        assert!(!shell.help_visible());
    }

    #[test]
    fn help_changes_with_the_active_workspace() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();

        shell.handle_key(
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
            &state,
        );
        let trains_help = super::help_lines(&shell, &state).join("\n");
        assert!(trains_help.contains("Current · Trains"));
        assert!(trains_help.contains("d Dispatch selected READY Train"));
        assert!(trains_help.contains("r Rename selected Train"));

        shell.handle_key(
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
            &state,
        );
        let market_help = super::help_lines(&shell, &state).join("\n");
        assert!(market_help.contains("Current · Market"));
        assert!(market_help.contains("Enter Buy selected Train"));

        shell.handle_key(
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            &state,
        );
        shell.handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            &state,
        );
        let services_help = super::help_lines(&shell, &state).join("\n");
        assert!(services_help.contains("Current · Passenger Services"));
        assert!(services_help.contains("n Create the first directional Passenger Service"));
    }

    #[test]
    fn bankruptcy_blocks_normal_actions_but_allows_exit_and_confirmed_safe_restart() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::ZERO;
        let mut shell = Shell::new();

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert_eq!(shell.active_view(), View::Map);
        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert_eq!(
            shell.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            ShellAction::RestartAfterBankruptcy
        );
        assert!(super::overlays::bankruptcy_text(true).contains("archive backup"));
        assert!(super::overlays::bankruptcy_text(true).contains("Press Enter to confirm"));

        let mut exiting_shell = Shell::new();
        assert_eq!(
            exiting_shell.handle_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Exit
        );
    }
