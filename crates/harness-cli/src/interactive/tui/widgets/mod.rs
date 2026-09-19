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
                },
                theme,
            ),
            Some(crate::interactive::events::Modal::Picker { items, selected }) => {
                picker::render(frame, area, items, *selected, theme);
            }
            Some(crate::interactive::events::Modal::Overlay { title, lines }) => {
                help::render(frame, area, title, lines, theme);
            }
            None => {}
        }
    }
    composer::render(frame, plan, state, theme);
    status::render(frame, plan.status, state, theme);
}
