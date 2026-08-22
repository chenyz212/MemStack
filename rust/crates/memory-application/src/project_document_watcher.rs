//! 项目文档文件监听：监听当前进程已绑定的工作空间，及时补同步数据库镜像。
//!
//! 监听只是及时性优化。真正的一致性仍由 `ProjectDocumentService::handoff`
//! 每次执行的全量校验与恢复保证，因此监听线程失败不会阻断主流程。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use memory_domain::{ALL_PROJECT_DOCUMENT_TYPES, BusinessError, ProjectDocumentType, content_checksum};
use memory_platform::{NamedMutex, WaitResult};

use crate::project_document_fs::{WorkspacePaths, document_checksum, read_text_if_exists};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const STABLE_SNAPSHOT_COUNT: u8 = 2;

type Snapshot = BTreeMap<ProjectDocumentType, Option<String>>;
type SyncCallback = Arc<dyn Fn() + Send + Sync + 'static>;

struct WatchControl {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

static REGISTRY: OnceLock<Mutex<BTreeMap<PathBuf, Weak<WatchControl>>>> = OnceLock::new();

/// 当前服务实例持有的监听租约；最后一个租约释放时停止底层线程。
pub struct WatchLease {
    root: PathBuf,
    control: Arc<WatchControl>,
}

impl Drop for WatchLease {
    fn drop(&mut self) {
        if Arc::strong_count(&self.control) > 1 {
            return;
        }
        let registry = REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()));
        let mut entries = registry.lock().expect("项目文档监听注册表已中毒");
        entries.remove(&self.root);
        drop(entries);
        self.control.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.control.thread.lock().expect("项目文档监听线程锁已中毒").take() {
            let _ = thread.join();
        }
    }
}

/// 注册或复用一个工作空间监听器。
pub fn register(paths: &WorkspacePaths, callback: SyncCallback) -> Result<WatchLease, BusinessError> {
    let root = paths.root().to_path_buf();
    let registry = REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut entries = registry.lock().expect("项目文档监听注册表已中毒");
    if let Some(control) = entries.get(&root).and_then(Weak::upgrade) {
        return Ok(WatchLease { root, control });
    }
    let control = Arc::new(WatchControl {
        stop: Arc::new(AtomicBool::new(false)),
        thread: Mutex::new(None),
    });
    let stop = Arc::clone(&control.stop);
    let thread_paths = paths.clone();
    let thread = std::thread::Builder::new()
        .name("memstack-project-document-watcher".to_string())
        .spawn(move || run_as_leader(stop, thread_paths, callback))
        .map_err(|error| {
            BusinessError::with_message(
                memory_domain::ErrorCode::InternalError,
                format!("启动项目文档监听失败：{error}"),
            )
        })?;
    *control.thread.lock().expect("项目文档监听线程锁已中毒") = Some(thread);
    entries.insert(root.clone(), Arc::downgrade(&control));
    Ok(WatchLease { root, control })
}

/// 跨进程竞争监听领导权；仅持锁进程读取文件，原进程退出后等待者自动接管。
fn run_as_leader(stop: Arc<AtomicBool>, paths: WorkspacePaths, callback: SyncCallback) {
    let mutex = match NamedMutex::create(&watcher_mutex_name(&paths)) {
        Ok(mutex) => mutex,
        Err(error) => {
            eprintln!("[project-document-watcher] 创建跨进程监听锁失败：{}", error.message);
            return;
        }
    };
    while !stop.load(Ordering::Relaxed) {
        match mutex.wait(POLL_INTERVAL.as_millis() as u32) {
            Ok(WaitResult::Acquired | WaitResult::Abandoned) => {
                watch_loop(Arc::clone(&stop), paths, callback);
                if let Err(error) = mutex.release() {
                    eprintln!("[project-document-watcher] 释放跨进程监听锁失败：{}", error.message);
                }
                return;
            }
            Ok(WaitResult::Timeout) => {}
            Err(error) => {
                eprintln!("[project-document-watcher] 等待跨进程监听锁失败：{}", error.message);
                return;
            }
        }
    }
}

/// 生成当前工作空间的跨进程监听锁名。
fn watcher_mutex_name(paths: &WorkspacePaths) -> String {
    format!(
        r"Local\MemStack.ProjectDocument.Watcher.{}",
        content_checksum(&paths.root().to_string_lossy())
    )
}

/// 生成当前工作空间的跨进程同步锁名，供交接与监听回调共同使用。
pub(crate) fn sync_mutex_name(paths: &WorkspacePaths) -> String {
    format!(
        r"Local\MemStack.ProjectDocument.Sync.{}",
        content_checksum(&paths.root().to_string_lossy())
    )
}

fn watch_loop(stop: Arc<AtomicBool>, paths: WorkspacePaths, callback: SyncCallback) {
    let initial = snapshot(&paths).ok();
    let mut observed = initial.clone();
    let mut processed = initial;
    let mut stable = 0u8;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL_INTERVAL);
        let Ok(current) = snapshot(&paths) else {
            stable = 0;
            continue;
        };
        if observed.as_ref() != Some(&current) {
            observed = Some(current);
            stable = 1;
            continue;
        }
        if processed == observed {
            stable = 0;
            continue;
        }
        stable = stable.saturating_add(1);
        if stable >= STABLE_SNAPSHOT_COUNT {
            callback();
            let synchronized = snapshot(&paths).ok();
            processed = synchronized.clone();
            observed = synchronized;
            stable = 0;
        }
    }
}

fn snapshot(paths: &WorkspacePaths) -> Result<Snapshot, BusinessError> {
    let mut result = BTreeMap::new();
    for document_type in ALL_PROJECT_DOCUMENT_TYPES {
        let content = read_text_if_exists(&paths.document_path(document_type))?;
        result.insert(document_type, content.map(|value| document_checksum(&value)));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_changes_when_document_is_modified_or_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::resolve(directory.path().to_str().unwrap()).unwrap();
        paths.ensure_memstack_dirs().unwrap();
        let document = paths.document_path(ProjectDocumentType::Context);
        std::fs::write(&document, "第一版").unwrap();
        let first = snapshot(&paths).unwrap();
        std::fs::write(&document, "第二版").unwrap();
        let second = snapshot(&paths).unwrap();
        assert_ne!(first, second);
        std::fs::remove_file(document).unwrap();
        let third = snapshot(&paths).unwrap();
        assert_ne!(second, third);
        assert_eq!(third[&ProjectDocumentType::Context], None);
    }

    #[test]
    fn registry_reuses_watcher_and_stops_after_last_lease() {
        let directory = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::resolve(directory.path().to_str().unwrap()).unwrap();
        let callback: SyncCallback = Arc::new(|| {});
        let first = register(&paths, Arc::clone(&callback)).unwrap();
        let second = register(&paths, callback).unwrap();
        assert!(Arc::ptr_eq(&first.control, &second.control));
        let stop = Arc::clone(&second.control.stop);
        drop(first);
        assert!(!stop.load(Ordering::Relaxed));
        drop(second);
        assert!(stop.load(Ordering::Relaxed));
    }
}
