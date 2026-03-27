use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct MonotonicInstant(InstantImpl);

#[cfg(not(target_arch = "wasm32"))]
type InstantImpl = std::time::Instant;

#[cfg(target_arch = "wasm32")]
type InstantImpl = f64;

impl MonotonicInstant {
    #[must_use]
    pub fn now() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self(std::time::Instant::now())
        }

        #[cfg(target_arch = "wasm32")]
        {
            let millis = boon::zoon::web_sys::window()
                .and_then(|window| window.performance())
                .map(|performance| performance.now())
                .unwrap_or_else(boon::zoon::js_sys::Date::now);
            Self(millis)
        }
    }

    #[must_use]
    pub fn elapsed(self) -> Duration {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.0.elapsed()
        }

        #[cfg(target_arch = "wasm32")]
        {
            let now = boon::zoon::web_sys::window()
                .and_then(|window| window.performance())
                .map(|performance| performance.now())
                .unwrap_or_else(boon::zoon::js_sys::Date::now);
            let millis = (now - self.0).max(0.0);
            Duration::from_secs_f64(millis / 1000.0)
        }
    }
}
