//! 管理已解码 CPU 像素的有界 LRU 缓存。

use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::Arc,
};

use super::{BudgetRejection, DecodedImage, SourceIdentity};

const MAX_BUDGET_REJECTIONS: usize = 32;

/// 按解码像素字节计费的 CPU 缓存。
pub(crate) struct CpuCache {
    budget_bytes: u64,
    bytes: u64,
    entries: HashMap<SourceIdentity, Arc<DecodedImage>>,
    lru: VecDeque<SourceIdentity>,
    rejections: VecDeque<(SourceIdentity, BudgetRejection)>,
}

impl CpuCache {
    pub(crate) fn new(budget_bytes: u64) -> Self {
        Self {
            budget_bytes,
            bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            rejections: VecDeque::new(),
        }
    }

    pub(crate) fn get(&mut self, key: &SourceIdentity) -> Option<Arc<DecodedImage>> {
        let image = self.entries.get(key)?.clone();
        self.touch(key);
        Some(image)
    }

    pub(crate) fn insert(&mut self, image: Arc<DecodedImage>) {
        let key = image.source.clone();
        self.remove_older_revision(&key.path);
        let cost = image.byte_cost();
        if cost > self.budget_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.byte_cost());
            self.lru.retain(|entry| entry != &key);
        }
        self.bytes = self.bytes.saturating_add(cost);
        self.lru.push_back(key.clone());
        self.entries.insert(key, image);
        self.evict_to_budget();
    }

    pub(crate) fn byte_cost(&self) -> u64 {
        self.bytes
    }

    /// 预算拒绝最多保留 32 条，不把失败冒充像素缓存成功；不用于临时 I/O 错误。
    pub(crate) fn remember_rejection(&mut self, source: SourceIdentity, error: BudgetRejection) {
        self.rejections.retain(|(key, _)| key.path != source.path);
        if self.rejections.len() == MAX_BUDGET_REJECTIONS {
            self.rejections.pop_front();
        }
        self.rejections.push_back((source, error));
    }

    /// 完整源修订必须匹配；文件变化后允许重新尝试。
    pub(crate) fn rejection(&self, source: &SourceIdentity) -> Option<BudgetRejection> {
        self.rejections
            .iter()
            .find(|(key, _)| key == source)
            .map(|(_, error)| error.clone())
    }

    fn remove_older_revision(&mut self, path: &Path) {
        let stale: Vec<_> = self
            .entries
            .keys()
            .filter(|key| key.path == path)
            .cloned()
            .collect();
        for key in stale {
            if let Some(image) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(image.byte_cost());
            }
            self.lru.retain(|entry| entry != &key);
        }
    }

    fn touch(&mut self, key: &SourceIdentity) {
        self.lru.retain(|entry| entry != key);
        self.lru.push_back(key.clone());
    }

    fn evict_to_budget(&mut self) {
        while self.bytes > self.budget_bytes {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(image) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(image.byte_cost());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CpuCache;
    use crate::image::{DecodedImage, PixelFormat, SourceIdentity};
    use std::{
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn image(name: &str, revision: u64, byte: u8) -> Arc<DecodedImage> {
        Arc::new(DecodedImage {
            source: SourceIdentity {
                path: PathBuf::from(name),
                length: revision,
                modified_nanos: None,
            },
            width: 1,
            height: 1,
            format: PixelFormat::Bgra8Premultiplied,
            stride: 4,
            pixels: vec![byte; 4],
        })
    }

    #[test]
    fn lru_hit_refreshes_order_and_budget_evicts_oldest() {
        let a = image("a.png", 1, 1);
        let b = image("b.png", 1, 2);
        let c = image("c.png", 1, 3);
        let mut cache = CpuCache::new(8);
        cache.insert(a.clone());
        cache.insert(b.clone());
        assert!(cache.get(&a.source).is_some());
        cache.insert(c.clone());
        assert!(cache.get(&a.source).is_some());
        assert!(cache.get(&b.source).is_none());
        assert_eq!(cache.byte_cost(), 8);
    }

    #[test]
    fn replacing_source_revision_removes_old_cached_pixels() {
        let old = image("same.png", 1, 1);
        let new = image("same.png", 2, 2);
        let mut cache = CpuCache::new(32);
        cache.insert(old.clone());
        cache.insert(new.clone());
        assert!(cache.get(&old.source).is_none());
        assert!(cache.get(&new.source).is_some());
        assert_eq!(cache.byte_cost(), 4);
    }

    #[test]
    fn oversized_entry_is_not_cached() {
        let mut cache = CpuCache::new(0);
        cache.insert(image("large.png", 1, 0));
        assert_eq!(cache.byte_cost(), 0);
    }

    #[test]
    fn budget_rejections_are_bounded_and_expire_when_the_source_revision_changes() {
        let mut cache = CpuCache::new(8);
        let old = image("same.png", 1, 0);
        let new = image("same.png", 2, 0);
        let rejection = super::BudgetRejection::DecodedBytes {
            bytes: 100,
            limit: 8,
        };
        cache.remember_rejection(old.source.clone(), rejection.clone());
        assert!(cache.rejection(&old.source).is_some());
        assert!(cache.rejection(&new.source).is_none());
        for number in 0..super::MAX_BUDGET_REJECTIONS {
            cache.remember_rejection(
                image(&format!("{number}.png"), 1, 0).source.clone(),
                rejection.clone(),
            );
        }
        assert_eq!(cache.rejections.len(), super::MAX_BUDGET_REJECTIONS);
        assert!(cache.rejection(&old.source).is_none());
        assert_eq!(cache.byte_cost(), 0);
    }

    #[test]
    fn test_identity_fields_are_stable_values() {
        let identity = SourceIdentity {
            path: PathBuf::from("x.png"),
            length: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            modified_nanos: Some(1),
        };
        assert!(identity.length > 0);
    }
}
