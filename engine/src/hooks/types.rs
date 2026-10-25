//! Types, timing phases, and return controls for hook dispatchers.

/// Hook execution timing phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookTiming {
    /// Executed before the original GameDLL or engine function.
    /// Allows mutating arguments or superceding the original execution.
    Pre,
    /// Executed after the original GameDLL or engine function.
    Post,
}

/// Action control returned from pre-hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookResult<T = ()> {
    /// Proceed with normal execution.
    Ignored,
    /// Mark event as handled (does not block original function).
    Handled,
    /// Block original function call and return a custom result value.
    Supercede(T),
    /// Block original function call without returning a value (void functions).
    Break,
}

impl<T> HookResult<T> {
    /// Returns `true` if the hook requested superceding original function.
    pub fn is_superceded(&self) -> bool {
        matches!(self, Self::Supercede(_) | Self::Break)
    }
}
