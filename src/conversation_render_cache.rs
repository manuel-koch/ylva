//! Derived rows for one active conversation and its independently sized header.

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Arc;

use ratatui::text::{Line, Span};
use uuid::Uuid;

/// Optional collapsed layouts only; visible layouts are never budget-evicted.
pub const COLLAPSED_BODY_BUDGET_BYTE_SIZE: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum EntrySlot {
    Entry(usize),
    Chrome(usize),
    Header,
    HeaderChrome,
    Thinking,
    ThinkingChrome,
    Response,
    Queued(usize),
    Intro,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct LayoutKey {
    pub slot: EntrySlot,
    pub revision: u64,
    pub width: u16,
    pub theme: String,
    pub collapsed: bool,
    pub title: Option<String>,
    pub animation: Option<(u8, u64)>,
}

/// Base styles are immutable. Hit ranges are entry-relative, before separators.
#[derive(Debug, Default)]
pub struct EntryLayout {
    pub rows: Vec<Line<'static>>,
    pub selection: Vec<String>,
    pub prefixes: Vec<usize>,
    pub tools: Vec<(usize, usize, Uuid)>,
    pub thinking: Vec<(usize, usize, usize)>,
}

impl EntryLayout {
    /// Capacity-based allocation estimate, not a resident-memory measurement.
    pub fn allocated_bytes(&self) -> usize {
        size_of::<Self>()
            + self.rows.capacity() * size_of::<Line<'static>>()
            + self
                .rows
                .iter()
                .map(|line| {
                    line.spans.capacity() * size_of::<Span<'static>>()
                        + line
                            .spans
                            .iter()
                            .map(|span| match &span.content {
                                std::borrow::Cow::Owned(text) => text.capacity(),
                                std::borrow::Cow::Borrowed(_) => 0,
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self.selection.capacity() * size_of::<String>()
            + self.selection.iter().map(String::capacity).sum::<usize>()
            + self.prefixes.capacity() * size_of::<usize>()
            + self.tools.capacity() * size_of::<(usize, usize, Uuid)>()
            + self.thinking.capacity() * size_of::<(usize, usize, usize)>()
    }
}

#[derive(Debug, Default)]
pub struct RowIndex {
    pub layouts: Vec<Arc<EntryLayout>>,
    ends: Vec<usize>,
    pub height: usize,
    trim_blank: bool,
}

impl RowIndex {
    pub fn replace(&mut self, layouts: Vec<Arc<EntryLayout>>, trim_blank: bool) -> bool {
        if self.trim_blank == trim_blank
            && self.layouts.len() == layouts.len()
            && self
                .layouts
                .iter()
                .zip(&layouts)
                .all(|(old, new)| Arc::ptr_eq(old, new))
        {
            return false;
        }
        self.layouts = layouts;
        self.trim_blank = trim_blank;
        self.ends.clear();
        let mut height = 0;
        for layout in &self.layouts {
            height += layout.rows.len();
            self.ends.push(height);
        }
        if trim_blank
            && self
                .layouts
                .last()
                .and_then(|layout| layout.rows.last())
                .is_some_and(|line| line.spans.is_empty())
        {
            height = height.saturating_sub(1);
        }
        self.height = height;
        true
    }

    pub fn visible_rows(&self, offset: usize, height: usize) -> Vec<Line<'static>> {
        (offset..offset.saturating_add(height).min(self.height))
            .map(|row| {
                let index = self.ends.partition_point(|end| *end <= row);
                let start = if index == 0 { 0 } else { self.ends[index - 1] };
                self.layouts[index].rows[row - start].clone()
            })
            .collect()
    }
}

#[derive(Debug)]
struct RetainedLayout {
    key: LayoutKey,
    layout: Arc<EntryLayout>,
    used_at: u64,
    bytes: usize,
}

#[derive(Debug, Default)]
pub struct ConversationRenderCache {
    active_view: Option<Option<Uuid>>,
    current: HashMap<EntrySlot, (LayoutKey, Arc<EntryLayout>)>,
    optional: Vec<RetainedLayout>,
    retained_bytes: usize,
    clock: u64,
    pub conversation: RowIndex,
    pub header: RowIndex,
    /// Per-instance deterministic instrumentation; no global counters or timers.
    pub layout_builds: usize,
    pub body_layout_builds: usize,
    pub painted_rows: usize,
}

impl ConversationRenderCache {
    pub fn clear(&mut self) {
        self.current = HashMap::new();
        self.optional = Vec::new();
        self.retained_bytes = 0;
        self.conversation = RowIndex::default();
        self.header = RowIndex::default();
        self.active_view = None;
    }

    pub fn activate(&mut self, view: Option<Uuid>) {
        if self.active_view != Some(view) {
            self.clear();
            self.active_view = Some(view);
        }
        self.painted_rows = 0;
    }

    pub fn retain_slots(&mut self, slots: &[EntrySlot]) {
        self.current.retain(|slot, _| slots.contains(slot));
        self.optional.retain(|entry| {
            let keep = slots.contains(&entry.key.slot);
            if !keep {
                self.retained_bytes -= entry.bytes;
            }
            keep
        });
    }

    pub fn get_or_build(
        &mut self,
        key: LayoutKey,
        build: impl FnOnce() -> EntryLayout,
    ) -> Arc<EntryLayout> {
        self.clock += 1;
        if let Some((old_key, layout)) = self.current.get(&key.slot) {
            if old_key == &key {
                return Arc::clone(layout);
            }
        }
        let compatible = |old: &LayoutKey| {
            old.slot == key.slot
                && old.revision == key.revision
                && old.width == key.width
                && old.theme == key.theme
                && old.title == key.title
        };
        self.optional.retain(|entry| {
            let keep = entry.key.slot != key.slot || compatible(&entry.key);
            if !keep {
                self.retained_bytes -= entry.bytes;
            }
            keep
        });
        let reused = self
            .optional
            .iter()
            .position(|entry| entry.key == key)
            .map(|index| {
                let entry = self.optional.remove(index);
                self.retained_bytes -= entry.bytes;
                entry.layout
            });
        if let Some((old_key, old_layout)) = self.current.remove(&key.slot) {
            if key.collapsed && !old_key.collapsed && compatible(&old_key) {
                let bytes = old_layout.allocated_bytes();
                if bytes <= COLLAPSED_BODY_BUDGET_BYTE_SIZE {
                    while self.retained_bytes + bytes > COLLAPSED_BODY_BUDGET_BYTE_SIZE {
                        let index = self
                            .optional
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, entry)| entry.used_at)
                            .map(|(index, _)| index)
                            .expect("retained layout exceeds budget");
                        self.retained_bytes -= self.optional.remove(index).bytes;
                    }
                    self.retained_bytes += bytes;
                    self.optional.push(RetainedLayout {
                        key: old_key,
                        layout: old_layout,
                        used_at: self.clock,
                        bytes,
                    });
                }
            }
        }
        let layout = reused.unwrap_or_else(|| {
            self.layout_builds += 1;
            if !matches!(
                key.slot,
                EntrySlot::Chrome(_) | EntrySlot::HeaderChrome | EntrySlot::ThinkingChrome
            ) {
                self.body_layout_builds += 1;
            }
            Arc::new(build())
        });
        self.current.insert(key.slot, (key, Arc::clone(&layout)));
        layout
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(slot: EntrySlot) -> LayoutKey {
        LayoutKey {
            slot,
            revision: 0,
            width: 80,
            theme: "dark".into(),
            collapsed: false,
            title: None,
            animation: None,
        }
    }

    fn layout() -> EntryLayout {
        EntryLayout {
            rows: vec![
                Line::from(vec![Span::raw("owned".to_string()), Span::raw("static")]),
                Line::default(),
            ],
            selection: vec!["ownedstatic".into(), String::new()],
            prefixes: vec![0, 0],
            tools: vec![(0, 0, Uuid::nil())],
            thinking: vec![(0, 0, 1)],
        }
    }

    #[test]
    fn shares_immutable_storage_and_replaces_every_key_dependency() {
        let mut cache = ConversationRenderCache::default();
        cache.activate(None);
        let original_key = key(EntrySlot::Entry(0));
        let original = cache.get_or_build(original_key.clone(), layout);
        assert!(original.allocated_bytes() > size_of::<EntryLayout>());
        let reused = cache.get_or_build(original_key.clone(), || panic!("warm build"));
        assert!(Arc::ptr_eq(&original, &reused));
        let changes = [
            LayoutKey {
                revision: 1,
                ..original_key.clone()
            },
            LayoutKey {
                width: 81,
                ..original_key.clone()
            },
            LayoutKey {
                theme: "light".into(),
                ..original_key.clone()
            },
            LayoutKey {
                title: Some("title".into()),
                ..original_key.clone()
            },
            LayoutKey {
                animation: Some((1, 10)),
                ..original_key.clone()
            },
        ];
        for changed in changes {
            let next = cache.get_or_build(changed, layout);
            assert!(!Arc::ptr_eq(&original, &next));
            assert_eq!(cache.current.len(), 1);
        }
        cache.get_or_build(key(EntrySlot::Header), layout);
        assert_eq!(cache.current.len(), 2);
        cache.activate(Some(Uuid::new_v4()));
        assert!(cache.current.is_empty());
        assert!(cache.conversation.layouts.is_empty());
        assert!(cache.header.layouts.is_empty());
    }

    #[test]
    fn exact_rows_ranges_and_visible_copy_bounds() {
        let first = Arc::new(layout());
        let second = Arc::new(layout());
        let mut index = RowIndex::default();
        assert!(index.replace(vec![Arc::clone(&first), Arc::clone(&second)], true));
        assert_eq!(index.height, 3);
        assert_eq!(index.visible_rows(1, usize::MAX).len(), 2);
        assert_eq!(index.visible_rows(2, 1)[0], second.rows[0]);
        assert!(index.visible_rows(100, 10).is_empty());
        assert!(!index.replace(vec![first, second], true));
        assert!(index.replace(vec![], false));
        assert_eq!(index.height, 0);
    }

    fn layout_with_byte_size(byte_size: usize) -> EntryLayout {
        let mut layout = EntryLayout {
            selection: Vec::with_capacity(1),
            ..Default::default()
        };
        let overhead = layout.allocated_bytes();
        layout
            .selection
            .push(String::with_capacity(byte_size - overhead));
        assert_eq!(layout.allocated_bytes(), byte_size);
        layout
    }

    #[test]
    fn optional_budget_boundary_and_oversized_active_layout() {
        let mut cache = ConversationRenderCache::default();
        let expanded = key(EntrySlot::Entry(0));
        let required = cache.get_or_build(expanded.clone(), || {
            layout_with_byte_size(COLLAPSED_BODY_BUDGET_BYTE_SIZE)
        });
        cache.get_or_build(
            LayoutKey {
                collapsed: true,
                ..expanded.clone()
            },
            layout,
        );
        assert_eq!(cache.retained_bytes(), COLLAPSED_BODY_BUDGET_BYTE_SIZE);
        let reused = cache.get_or_build(expanded.clone(), || panic!("retained body"));
        assert!(Arc::ptr_eq(&required, &reused));
        assert_eq!(cache.retained_bytes(), 0);
        let oversized_key = LayoutKey {
            revision: 1,
            ..expanded
        };
        cache.get_or_build(oversized_key.clone(), || {
            layout_with_byte_size(COLLAPSED_BODY_BUDGET_BYTE_SIZE + 1)
        });
        cache.get_or_build(oversized_key.clone(), || {
            panic!("oversized active reparsed")
        });
        cache.get_or_build(
            LayoutKey {
                collapsed: true,
                ..oversized_key
            },
            layout,
        );
        assert_eq!(cache.retained_bytes(), 0);
    }

    #[test]
    fn header_and_list_share_lru_budget_and_prune_obsolete_entries() {
        let mut cache = ConversationRenderCache::default();
        let half_budget = COLLAPSED_BODY_BUDGET_BYTE_SIZE / 2;
        let first = key(EntrySlot::Entry(0));
        let second = key(EntrySlot::Header);
        let third = key(EntrySlot::Entry(1));
        for expanded in [&first, &second] {
            cache.get_or_build(expanded.clone(), || layout_with_byte_size(half_budget));
            cache.get_or_build(
                LayoutKey {
                    collapsed: true,
                    ..expanded.clone()
                },
                layout,
            );
        }
        assert_eq!(cache.retained_bytes(), COLLAPSED_BODY_BUDGET_BYTE_SIZE);
        cache.get_or_build(first.clone(), || panic!("reuse updates LRU"));
        cache.get_or_build(
            LayoutKey {
                collapsed: true,
                ..first.clone()
            },
            layout,
        );
        cache.get_or_build(third.clone(), || layout_with_byte_size(half_budget));
        cache.get_or_build(
            LayoutKey {
                collapsed: true,
                ..third
            },
            layout,
        );
        assert!(cache
            .optional
            .iter()
            .all(|entry| entry.key.slot != EntrySlot::Header));
        assert!(cache
            .optional
            .iter()
            .any(|entry| entry.key.slot == EntrySlot::Entry(0)));
        cache.get_or_build(
            LayoutKey {
                revision: 1,
                ..first
            },
            layout,
        );
        assert_eq!(cache.optional.len(), 1);
        cache.retain_slots(&[EntrySlot::Header]);
        assert!(cache.optional.is_empty());
        assert_eq!(cache.retained_bytes(), 0);
        cache.clear();
        assert!(cache.current.is_empty());
    }

    #[test]
    fn width_change_prunes_optional_body_and_view_switch_releases_indexes() {
        let mut cache = ConversationRenderCache::default();
        cache.activate(None);
        let expanded = key(EntrySlot::Entry(0));
        let body = cache.get_or_build(expanded.clone(), layout);
        cache.conversation.replace(vec![body], false);
        cache.get_or_build(
            LayoutKey {
                collapsed: true,
                ..expanded.clone()
            },
            layout,
        );
        cache.get_or_build(
            LayoutKey {
                width: 40,
                collapsed: true,
                ..expanded
            },
            layout,
        );
        assert!(cache.optional.is_empty());
        cache.activate(None);
        assert!(!cache.current.is_empty());
        cache.activate(Some(Uuid::nil()));
        assert!(cache.current.is_empty());
        assert!(cache.conversation.layouts.is_empty());
        assert_eq!(cache.retained_bytes(), 0);
    }
}
