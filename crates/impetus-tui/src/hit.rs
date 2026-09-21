//! Mouse hit-testing for the TUI.
//!
//! Render records [`HitTarget`]s each frame; input resolves `(x, y)` → action.
//! Later targets win (overlays paint after chrome).

use std::time::{Duration, Instant};

/// Screen rectangle in terminal cells (ratatui-compatible, no ratatui dep).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectHit {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl RectHit {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(self, column: u16, row: u16) -> bool {
        column >= self.x
            && row >= self.y
            && column < self.x.saturating_add(self.width)
            && row < self.y.saturating_add(self.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    Composer,
    TimelineItem { index: usize },
    SessionPickerRow { index: usize },
    SessionPanelRow { index: usize },
    ApprovalAccept,
    ApprovalReject,
    ApprovalInspect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitTarget {
    pub rect: RectHit,
    pub kind: HitKind,
}

/// Last mouse press — used for double-click expand/collapse.
#[derive(Clone, Copy, Debug)]
pub struct PointerClick {
    pub at: Instant,
    pub column: u16,
    pub row: u16,
    pub kind: HitKind,
}

pub const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// Resolve topmost target under the pointer (reverse paint order).
pub fn resolve_hit(targets: &[HitTarget], column: u16, row: u16) -> Option<HitKind> {
    targets
        .iter()
        .rev()
        .find(|target| target.rect.contains(column, row))
        .map(|target| target.kind)
}

/// True when this press is a double-click on the same kind at the same cell.
pub fn is_double_click(
    previous: Option<&PointerClick>,
    column: u16,
    row: u16,
    kind: HitKind,
    now: Instant,
) -> bool {
    previous.is_some_and(|prev| {
        now.duration_since(prev.at) <= DOUBLE_CLICK
            && prev.column == column
            && prev.row == row
            && prev.kind == kind
    })
}

/// Cycle composer intent: Prompt → Steer → FollowUp → Prompt.
pub fn cycle_prompt_intent(
    current: impetus_client::protocol::UserPromptIntent,
) -> impetus_client::protocol::UserPromptIntent {
    use impetus_client::protocol::UserPromptIntent::{FollowUp, Prompt, Steer};
    match current {
        Prompt => Steer,
        Steer => FollowUp,
        FollowUp => Prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impetus_client::protocol::UserPromptIntent;

    #[test]
    fn resolve_hit_prefers_later_targets() {
        let targets = [
            HitTarget {
                rect: RectHit::new(0, 0, 10, 5),
                kind: HitKind::Composer,
            },
            HitTarget {
                rect: RectHit::new(2, 1, 3, 1),
                kind: HitKind::TimelineItem { index: 3 },
            },
        ];
        assert_eq!(
            resolve_hit(&targets, 3, 1),
            Some(HitKind::TimelineItem { index: 3 })
        );
        assert_eq!(resolve_hit(&targets, 0, 0), Some(HitKind::Composer));
        assert_eq!(resolve_hit(&targets, 20, 20), None);
    }

    #[test]
    fn rect_hit_contains_edges_exclusive_max() {
        let rect = RectHit::new(5, 5, 2, 2);
        assert!(rect.contains(5, 5));
        assert!(rect.contains(6, 6));
        assert!(!rect.contains(7, 5));
        assert!(!rect.contains(5, 7));
    }

    #[test]
    fn cycle_prompt_intent_rotates_three_ways() {
        assert_eq!(
            cycle_prompt_intent(UserPromptIntent::Prompt),
            UserPromptIntent::Steer
        );
        assert_eq!(
            cycle_prompt_intent(UserPromptIntent::Steer),
            UserPromptIntent::FollowUp
        );
        assert_eq!(
            cycle_prompt_intent(UserPromptIntent::FollowUp),
            UserPromptIntent::Prompt
        );
    }

    #[test]
    fn is_double_click_requires_same_cell_kind_and_window() {
        let kind = HitKind::TimelineItem { index: 1 };
        let first = PointerClick {
            at: Instant::now(),
            column: 4,
            row: 8,
            kind,
        };
        assert!(is_double_click(
            Some(&first),
            4,
            8,
            kind,
            first.at + Duration::from_millis(100)
        ));
        assert!(!is_double_click(
            Some(&first),
            5,
            8,
            kind,
            first.at + Duration::from_millis(100)
        ));
        assert!(!is_double_click(
            Some(&first),
            4,
            8,
            kind,
            first.at + Duration::from_millis(600)
        ));
    }
}
