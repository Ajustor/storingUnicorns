use ratatui::layout::Rect;
use std::sync::{Arc, RwLock};

/// Type of clickable element
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickableType {
    Connection(usize),
    Schema(usize),
    Table { schema_idx: usize, table_idx: usize },
    QueryEditor,
    QueryTab(usize),
    ResultRow(usize),
    Panel(super::layout::PanelType),
}

/// A clickable area in the UI
#[derive(Debug, Clone)]
pub struct ClickableArea {
    pub rect: Rect,
    pub click_type: ClickableType,
}

impl ClickableArea {
    pub fn new(rect: Rect, click_type: ClickableType) -> Self {
        Self { rect, click_type }
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.rect.x
            && x < self.rect.x + self.rect.width
            && y >= self.rect.y
            && y < self.rect.y + self.rect.height
    }
}

/// Registry of all clickable areas - thread-safe for use across render and event handling
#[derive(Debug, Clone, Default)]
pub struct ClickableRegistry {
    areas: Arc<RwLock<Vec<ClickableArea>>>,
}

impl ClickableRegistry {
    pub fn new() -> Self {
        Self {
            areas: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Clear all registered areas (call at start of each render)
    pub fn clear(&self) {
        if let Ok(mut areas) = self.areas.write() {
            areas.clear();
        }
    }

    /// Register a clickable area
    pub fn register(&self, rect: Rect, click_type: ClickableType) {
        if let Ok(mut areas) = self.areas.write() {
            areas.push(ClickableArea::new(rect, click_type));
        }
    }

    /// Find the clickable area at the given position (returns the topmost/last registered)
    pub fn find_at(&self, x: u16, y: u16) -> Option<ClickableType> {
        if let Ok(areas) = self.areas.read() {
            // Iterate in reverse to get the topmost element first
            for area in areas.iter().rev() {
                if area.contains(x, y) {
                    return Some(area.click_type.clone());
                }
            }
        }
        None
    }

    /// Get the query editor area for cursor positioning
    pub fn get_query_editor_rect(&self) -> Option<Rect> {
        if let Ok(areas) = self.areas.read() {
            for area in areas.iter() {
                if matches!(area.click_type, ClickableType::QueryEditor) {
                    return Some(area.rect);
                }
            }
        }
        None
    }
}
