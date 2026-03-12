//! Scoped, colored terminal logger built on `tracing`.
//!
//! ```
//! let root = Logger::new("wasm-js-bridge");
//! let child = root.child("predicates");
//! child.step("Compiling to WASM…");
//! // stderr: [wasm-js-bridge] [predicates] → Compiling to WASM…
//! ```
//!
//! Call [`init`] once at startup to install the subscriber.

use colored::Colorize as _;
use tracing::Span;
use tracing_subscriber::fmt::{format::Writer, FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

// ---------------------------------------------------------------------------
// Subscriber setup
// ---------------------------------------------------------------------------

struct WjbFormat;

impl<S, N> FormatEvent<S, N> for WjbFormat
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        // Collect span names from outermost to innermost.
        let mut scopes: Vec<String> = Vec::new();
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                scopes.push(span.name().to_string());
            }
        }

        // Render scope chain: [foo] [bar]
        for name in &scopes {
            write!(writer, "{} ", format!("[{name}]").cyan())?;
        }

        // Severity indicator + color for the message.
        let level = *event.metadata().level();
        let indicator: &str = match level {
            tracing::Level::ERROR => "✖",
            tracing::Level::WARN => "⚠",
            tracing::Level::INFO => "→",
            _ => "·",
        };

        let indicator_colored = match level {
            tracing::Level::ERROR => indicator.red().bold().to_string(),
            tracing::Level::WARN => indicator.yellow().to_string(),
            tracing::Level::INFO => indicator.cyan().to_string(),
            _ => indicator.dimmed().to_string(),
        };

        write!(writer, "{indicator_colored} ")?;

        // Write the message fields (just the `message` field).
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Install the `tracing` subscriber. Call once at the start of `main`.
pub fn init() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .event_format(WjbFormat)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

// ---------------------------------------------------------------------------
// Logger
// ---------------------------------------------------------------------------

/// A scoped logger. Creates a `tracing` span; all messages emitted via this
/// logger appear under that span, producing `[scope1] [scope2] …` prefixes.
pub struct Logger {
    span: Span,
}

impl Logger {
    /// Create a root logger with the given scope name.
    pub fn new(name: &'static str) -> Self {
        Self {
            span: tracing::info_span!("{}", name),
        }
    }

    /// Create a child logger nested under this one.
    pub fn child(&self, name: &'static str) -> Self {
        let _guard = self.span.enter();
        Self {
            span: tracing::info_span!("{}", name),
        }
    }

    /// Informational step announcement.
    pub fn step(&self, msg: &str) {
        let _guard = self.span.enter();
        tracing::info!("{msg}");
    }

    /// Success / completion.
    pub fn done(&self, msg: &str) {
        let _guard = self.span.enter();
        // Emit at INFO but the caller conventionally uses this for completion.
        tracing::info!("{}", msg.green().bold());
    }

    /// Warning.
    pub fn warn(&self, msg: &str) {
        let _guard = self.span.enter();
        tracing::warn!("{msg}");
    }

    /// Error.
    pub fn error(&self, msg: &str) {
        let _guard = self.span.enter();
        tracing::error!("{msg}");
    }
}
