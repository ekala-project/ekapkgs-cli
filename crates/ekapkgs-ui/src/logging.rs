use std::fmt;

use tracing::Level;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use yansi::Paint;

/// Custom formatter that produces nh-style colored log prefixes:
/// - `>` (green) for info
/// - `!` (yellow) for warnings
/// - `ERROR` (red) for errors
/// - `DEBUG` (blue) for debug
struct PrefixFormatter;

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for PrefixFormatter
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();

        match *meta.level() {
            Level::ERROR => write!(writer, "{} ", "ERROR".red().bold())?,
            Level::WARN => write!(writer, "{} ", "!".yellow().bold())?,
            Level::INFO => write!(writer, "{} ", ">".green().bold())?,
            Level::DEBUG => write!(writer, "{} ", "DEBUG".blue())?,
            Level::TRACE => write!(writer, "{} ", "TRACE".dim())?,
        }

        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Initialize the logging subscriber with the colored prefix formatter.
///
/// Respects the `EKAPKGS_LOG` environment variable for filtering, defaulting
/// to the level determined by the verbosity flag.
pub fn init(verbosity: &clap_verbosity_flag::Verbosity) {
    let filter = EnvFilter::try_from_env("EKAPKGS_LOG").unwrap_or_else(|_| {
        let level = verbosity.log_level().map(|l| match l {
            log::Level::Error => "error",
            log::Level::Warn => "warn",
            log::Level::Info => "info",
            log::Level::Debug => "debug",
            log::Level::Trace => "trace",
        });
        EnvFilter::new(level.unwrap_or("info"))
    });

    let fmt_layer = tracing_subscriber::fmt::layer().event_format(PrefixFormatter);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();
}
