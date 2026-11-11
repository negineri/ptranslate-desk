//! デスクトップとChromeが共有するローカル推論の実行枠。
use std::sync::Mutex;
use tokio::sync::{Mutex as AsyncMutex, Notify};
pub const MAX_PARALLEL: usize = 64;
pub const CONTEXT_PER_SLOT: usize = 32768;
struct State {
    limit: usize,
    active: usize,
}
pub struct Slots {
    state: Mutex<State>,
    turn: AsyncMutex<()>,
    changed: Notify,
}
pub struct Permit<'a>(&'a Slots);
impl Slots {
    pub fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(State { limit, active: 0 }),
            turn: AsyncMutex::new(()),
            changed: Notify::new(),
        }
    }
    pub fn limit(&self) -> usize {
        self.state.lock().unwrap().limit
    }
    pub fn in_use(&self) -> bool {
        self.state.lock().unwrap().active != 0
    }
    pub fn resize(&self, limit: usize) {
        assert!((1..=MAX_PARALLEL).contains(&limit));
        self.state.lock().unwrap().limit = limit;
        self.changed.notify_one();
    }
    pub async fn acquire(&self) -> Result<Permit<'_>, ()> {
        // 待つ人の順番はTokioのFIFO mutexで保持する。待機futureの破棄で次へ渡る。
        let _turn = self.turn.lock().await;
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self.state.lock().unwrap();
                if state.active < state.limit {
                    state.active += 1;
                    return Ok(Permit(self));
                }
            }
            changed.await;
        }
    }
    // 待機中のローカル翻訳があれば、ワーカーは順番を追い越さない。
    pub fn try_acquire(&self) -> Result<Permit<'_>, ()> {
        let _turn = self.turn.try_lock().map_err(|_| ())?;
        let mut state = self.state.lock().unwrap();
        if state.active >= state.limit {
            return Err(());
        }
        state.active += 1;
        Ok(Permit(self))
    }
    #[cfg(test)]
    pub fn available_permits(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.limit.saturating_sub(state.active)
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().active -= 1;
        self.0.changed.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};
    #[tokio::test]
    async fn decreasing_limit_waits_for_existing_work_and_canceled_waiter_leaks_nothing() {
        let slots = Arc::new(Slots::new(8));
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(slots.acquire().await.unwrap());
        }
        slots.resize(2);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), slots.acquire())
                .await
                .is_err()
        );
        for _ in 0..6 {
            held.pop();
        }
        assert!(slots.try_acquire().is_err());
        held.pop();
        let permit = slots.acquire().await.unwrap();
        assert_eq!(slots.available_permits(), 0);
        drop(permit);
        drop(held);
        assert_eq!(slots.available_permits(), 2);
        slots.resize(12);
        let mut held = Vec::new();
        for _ in 0..12 {
            held.push(slots.acquire().await.unwrap());
        }
        assert!(slots.try_acquire().is_err());
    }
    #[tokio::test]
    async fn waiting_requests_are_fifo_and_canceling_the_head_wakes_the_next() {
        let slots = Arc::new(Slots::new(1));
        let held = slots.acquire().await.unwrap();
        let first = {
            let slots = slots.clone();
            tokio::spawn(async move {
                let _permit = slots.acquire().await.unwrap();
                std::future::pending::<()>().await;
            })
        };
        tokio::task::yield_now().await;
        let second = {
            let slots = slots.clone();
            tokio::spawn(async move {
                let _permit = slots.acquire().await.unwrap();
                2
            })
        };
        tokio::task::yield_now().await;
        first.abort();
        let _ = first.await;
        drop(held);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), second)
                .await
                .unwrap()
                .unwrap(),
            2
        );
        assert_eq!(slots.available_permits(), 1);
    }
}
