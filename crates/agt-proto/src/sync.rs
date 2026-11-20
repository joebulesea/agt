//! 不會因中毒（poisoned）而連鎖 panic 的鎖 helper。
//!
//! 標準函式庫的 `Mutex` / `RwLock` 在持鎖的執行緒 panic 後會「中毒」，之後每次
//! `lock().unwrap()` 都會跟著 panic。daemon 是常駐程式，一個執行緒的 panic 不該讓
//! 全部分頁一起失效，所以取鎖時直接取回內部資料（`PoisonError::into_inner`）。

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// `Mutex` 的取鎖：鎖中毒時仍回傳資料。
pub trait MutexExt<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `RwLock` 的取鎖：鎖中毒時仍回傳資料。
pub trait RwLockExt<T> {
    fn read_or_recover(&self) -> RwLockReadGuard<'_, T>;
    fn write_or_recover(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_or_recover(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(|e| e.into_inner())
    }
    fn write_or_recover(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn poisoned_mutex_still_usable() {
        let m = Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(m.is_poisoned());
        *m.lock_or_recover() += 1;
        assert_eq!(*m.lock_or_recover(), 2);
    }

    #[test]
    fn poisoned_rwlock_still_usable() {
        let l = Arc::new(RwLock::new(1));
        let l2 = l.clone();
        let _ = std::thread::spawn(move || {
            let _g = l2.write().unwrap();
            panic!("poison");
        })
        .join();
        *l.write_or_recover() += 1;
        assert_eq!(*l.read_or_recover(), 2);
    }
}
