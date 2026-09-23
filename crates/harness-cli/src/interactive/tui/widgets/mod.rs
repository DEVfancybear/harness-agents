//! Viewport widgets: composer, status bar and the temporary panels.
//!
//! Everything is drawn from the [`UiState`](crate::interactive::events::UiState)
//! snapshot, so a widget cannot reach back into the controller and a frame is a
//! pure function of the state plus the theme.

pub mod approval;
pub mod composer;
pub mod help;
pub mod picker;
pub mod status;
pub mod suggest;

use ratatui::Frame;

use super::layout::Plan;
use super::theme::Theme;
use crate::interactive::events::UiState;

/// Draw one frame.
pub fn render(frame: &mut Frame, plan: &Plan, state: &UiState, theme: &Theme) {
    if let Some(area) = plan.live {
        composer::render_live(frame, area, state, theme);
    }
    if let Some(area) = plan.modal {
        match state.modal.as_ref() {
            Some(crate::interactive::events::Modal::Approval {
                request_id,
                action,
                summary,
                workspace,
                scope,
                expires_at,
                read_only,
                scroll,
            }) => approval::render(
                frame,
                area,
                approval::Proposal {
                    request_id,
                    action,
                    summary,
                    workspace,
                    scope,
                    expires_at: *expires_at,
                    read_only: *read_only,
                    scroll: *scroll,
                },
                theme,
            ),
            Some(crate::interactive::events::Modal::Picker { items, selected }) => {
                picker::render(frame, area, items, *selected, theme);
            }
            Some(crate::interactive::events::Modal::FilePicker { items, selected }) => {
                picker::render_files(frame, area, items, *selected, theme);
            }
            Some(crate::interactive::events::Modal::Question { prompt, options }) => {
                let mut lines = vec![prompt.clone()];
                lines.extend(
                    options
                        .iter()
                        .enumerate()
                        .map(|(index, option)| format!("{}. {option}", index + 1)),
                );
                lines.push("hoặc nhập câu trả lời rồi nhấn Enter".to_owned());
                help::render(frame, area, "question", &lines, 0, theme);
            }
            Some(crate::interactive::events::Modal::Overlay {
                title,
                lines,
                scroll,
            }) => {
                help::render(frame, area, title, lines, *scroll, theme);
            }
            None => {}
        }
    }
    // The slash-command menu sits between the upper region and the composer: it
    // belongs to the draft being typed, not to the conversation above it.
    if let Some(area) = plan.suggest {
        suggest::render(frame, area, state, theme);
    }
    composer::render(frame, plan, state, theme);
    status::render(frame, plan.status, state, theme);
}
