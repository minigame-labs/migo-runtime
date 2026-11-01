// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::frames::{FrameResult, Id3v2Chapter, Id3v2TableOfContents};
use std::mem::size_of;
use symphonia_core::meta::{ChapterGroupItem, MetadataOptions, Tag, Visual};

/// Independent decoded-value, retained-revision, encoded-scratch and depth limits.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    tag: Option<usize>,
    visual: Option<usize>,
    pub total: Option<usize>,
    frame: Option<usize>,
    pub depth: Option<usize>,
}
impl Limits {
    pub fn new(options: MetadataOptions) -> Self {
        Self {
            tag: options.limit_tag_bytes.limit_or_default(64 * 1024),
            visual: options.limit_visual_bytes.limit_or_default(1024 * 1024),
            total: options.limit_total_bytes.limit_or_default(4 * 1024 * 1024),
            frame: options.limit_frame_bytes.limit_or_default(1024 * 1024),
            depth: options.limit_nesting_depth.limit_or_default(8),
        }
    }
    pub fn allow_frame(&self, size: u64, picture: bool, depth: usize) -> bool {
        self.total != Some(0)
            && !self.depth.is_some_and(|limit| depth > limit)
            && !self.frame.is_some_and(|limit| size > limit as u64)
            && if picture {
                self.visual != Some(0)
            } else {
                self.tag != Some(0)
            }
    }
    fn tag_allowed(&self, tag: &Tag) -> bool {
        !self
            .tag
            .is_some_and(|limit| tag.storage_size_upper_bound() > limit)
    }
    fn visual_allowed(&self, visual: &mut Visual) -> bool {
        if self.visual.is_some_and(|limit| visual.data.len() > limit) {
            return false;
        }
        visual.tags.retain(|tag| self.tag_allowed(tag));
        true
    }
    pub fn filter(&self, result: FrameResult) -> FrameResult {
        match result {
            FrameResult::Tag(tag) if !self.tag_allowed(&tag) => FrameResult::Skipped,
            FrameResult::MultipleTags(mut tags) => {
                tags.retain(|tag| self.tag_allowed(tag));
                FrameResult::MultipleTags(tags)
            }
            FrameResult::Visual(mut visual) => {
                if self.visual_allowed(&mut visual) {
                    FrameResult::Visual(visual)
                } else {
                    FrameResult::Skipped
                }
            }
            FrameResult::Chapter(mut value) => {
                value.chapter.tags.retain(|tag| self.tag_allowed(tag));
                value
                    .chapter
                    .visuals
                    .retain_mut(|visual| self.visual_allowed(visual));
                FrameResult::Chapter(value)
            }
            FrameResult::TableOfContents(mut value) => {
                value.tags.retain(|tag| self.tag_allowed(tag));
                value
                    .visuals
                    .retain_mut(|visual| self.visual_allowed(visual));
                FrameResult::TableOfContents(value)
            }
            other => other,
        }
    }
}

/// Charge conservative storage before adding it to the revision or chapter index.
/// Vec pushes may have up to four initial slots and geometric spare capacity;
/// charging four inline slots per entry covers that without changing containers.
pub(super) struct Retention {
    remaining: Option<usize>,
}
impl Retention {
    pub fn new(limits: Limits) -> Self {
        Self {
            remaining: limits.total,
        }
    }
    pub fn admit(&mut self, size: usize) -> bool {
        match self.remaining {
            Some(left) if size > left => false,
            Some(left) => {
                self.remaining = Some(left - size);
                true
            }
            None => true,
        }
    }
    pub fn tag(&mut self, value: &Tag) -> bool {
        self.admit(
            value
                .storage_size_upper_bound()
                .saturating_add(3 * size_of::<Tag>()),
        )
    }
    pub fn visual(&mut self, value: &Visual) -> bool {
        self.admit(
            value
                .storage_size_upper_bound()
                .saturating_add(3 * size_of::<Visual>()),
        )
    }
    pub fn chapter(&mut self, value: &Id3v2Chapter) -> bool {
        let size = container(&value.chapter.tags, &value.chapter.visuals)
            .saturating_add(4 * size_of::<Id3v2Chapter>() + 4 * size_of::<String>() + 32)
            .saturating_add(value.id.capacity().saturating_mul(2));
        self.admit(size)
    }
    pub fn toc(&mut self, value: &Id3v2TableOfContents) -> bool {
        let size = value.items.iter().fold(
            container(&value.tags, &value.visuals)
                .saturating_add(value.items.capacity() * (size_of::<String>() + size_of::<ChapterGroupItem>()))
                .saturating_add(
                    4 * size_of::<Id3v2TableOfContents>() + 4 * size_of::<String>() + 32,
                )
                .saturating_add(value.id.capacity().saturating_mul(3)),
            |size, item| size.saturating_add(item.capacity()),
        );
        self.admit(size)
    }
}
fn container(tags: &Vec<Tag>, visuals: &Vec<Visual>) -> usize {
    let size = tags
        .iter()
        .fold(tags.capacity() * size_of::<Tag>(), |size, value| {
            size.saturating_add(
                value
                    .storage_size_upper_bound()
                    .saturating_sub(size_of::<Tag>()),
            )
        });
    visuals.iter().fold(
        size.saturating_add(visuals.capacity() * size_of::<Visual>()),
        |size, value| {
            size.saturating_add(
                value
                    .storage_size_upper_bound()
                    .saturating_sub(size_of::<Visual>()),
            )
        },
    )
}
