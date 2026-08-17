//! Terminal rendering: the live chat stream, and pretty-printers for the read commands.
//!
//! `chat` renders deltas as they land rather than buffering, because the point of the
//! command is to make cadence and ordering directly observable (spec 06 §2).

use std::ffi::{c_char, c_void, CStr};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";
pub const CYAN: &str = "\x1b[36m";
pub const RED: &str = "\x1b[31m";
pub const RESET: &str = "\x1b[0m";

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Shared with the event callback through the opaque `ctx` pointer. An atomic rather than a
/// channel because the dispatcher thread sets it while `main` polls.
#[derive(Default)]
pub struct Stream {
    pub done: AtomicBool,
}

impl Stream {
    pub fn wait(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while !self.done.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        true
    }
}

/// The event callback. Runs on the core's dispatcher thread, one event at a time.
pub extern "C" fn on_event(json: *const c_char, _len: usize, ctx: *mut c_void) {
    let stream = unsafe { &*(ctx as *const Stream) };

    let text = unsafe { CStr::from_ptr(json) }.to_string_lossy();
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return;
    };

    match value["type"].as_str().unwrap_or("") {
        "message_update" => render_message_update(&value["event"]),
        "tool_execution_start" => print!(
            "\n{CYAN}→ {}{RESET}",
            value["toolName"].as_str().unwrap_or("?")
        ),
        "tool_execution_end" => {
            let failed = value["isError"].as_bool().unwrap_or(false);
            let summary = value["result"]["text"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value["result"].to_string());
            println!(
                "  {}{}{RESET}",
                if failed { RED } else { DIM },
                truncate(&summary, 72)
            );
        }
        "run_end" => {
            println!(
                "\n{DIM}{} · {} tokens · {}ms{RESET}",
                value["outcome"].as_str().unwrap_or("?"),
                value["usage"]["totalTokens"].as_u64().unwrap_or(0),
                value["durationMs"].as_u64().unwrap_or(0)
            );
            stream.done.store(true, Ordering::SeqCst);
        }
        "error" => eprintln!(
            "\n{RED}{}: {}{RESET}",
            value["code"].as_str().unwrap_or("error"),
            value["message"].as_str().unwrap_or("")
        ),
        _ => {}
    }
    let _ = std::io::stdout().flush();
}

fn render_message_update(event: &Value) {
    match event["type"].as_str().unwrap_or("") {
        "thinking_start" => print!("\n{DIM}"),
        "thinking_delta" | "text_delta" => print!("{}", event["delta"].as_str().unwrap_or("")),
        "thinking_end" => print!("{RESET}\n\n"),
        "toolcall_end" => print!(
            "\n{CYAN}⟡ {}{RESET}",
            event["toolCall"]["name"].as_str().unwrap_or("?")
        ),
        "error" => eprintln!(
            "\n{RED}{}{RESET}",
            event["error"]["errorMessage"].as_str().unwrap_or("failed")
        ),
        _ => {}
    }
}

// ---------------------------------------------------------------- read commands

/// Reads through `Value` rather than `UsageStats` on purpose: W3 is still growing the type,
/// and a debugging tool that stops compiling on every field addition is not much use.
pub fn print_stats(stats: &Value) {
    let headline = &stats["headline"];
    let row = |label: &str, value: String| println!("  {label:<22}{value}");

    println!(
        "{BOLD}usage · {}{RESET}",
        stats["range"].as_str().unwrap_or("?")
    );
    row("sessions", num(&headline["sessions"]));
    row("messages", num(&headline["messages"]));
    row("turns", num(&headline["turns"]));
    row("total tokens", num(&headline["totalTokens"]));
    row(
        "in / out",
        format!("{} / {}", num(&headline["input"]), num(&headline["output"])),
    );
    row(
        "cache read / write",
        format!(
            "{} / {}",
            num(&headline["cacheRead"]),
            num(&headline["cacheWrite"])
        ),
    );
    row(
        "cost",
        format!("${:.2}", headline["totalCost"].as_f64().unwrap_or(0.0)),
    );
    row(
        "active days / streak",
        format!(
            "{} / {}",
            num(&headline["activeDays"]),
            num(&headline["currentStreak"])
        ),
    );
    row(
        "peak hour",
        format!("{:02}:00", headline["peakHour"].as_u64().unwrap_or(0)),
    );

    if let Some(daily) = stats["daily"].as_array().filter(|d| !d.is_empty()) {
        let series: Vec<u64> = daily
            .iter()
            .map(|d| d["totalTokens"].as_u64().unwrap_or(0))
            .collect();
        println!("\n  {DIM}daily{RESET}  {}", sparkline(&series));
        println!(
            "  {DIM}{} … {}{RESET}",
            daily.first().unwrap()["date"].as_str().unwrap_or(""),
            daily.last().unwrap()["date"].as_str().unwrap_or("")
        );
    }
    if let Some(hourly) = stats["hourly"].as_array().filter(|h| !h.is_empty()) {
        let series: Vec<u64> = hourly
            .iter()
            .map(|h| h["totalTokens"].as_u64().unwrap_or(0))
            .collect();
        println!(
            "  {DIM}hourly{RESET} {}  {DIM}00 … 23{RESET}",
            sparkline(&series)
        );
    }
}

pub fn print_hits(hits: &Value) {
    let Some(hits) = hits.as_array() else {
        println!("{DIM}no results{RESET}");
        return;
    };
    if hits.is_empty() {
        println!("{DIM}no results{RESET}");
        return;
    }
    for hit in hits {
        println!(
            "{BOLD}{}{RESET} {DIM}{}{RESET}",
            hit["title"].as_str().unwrap_or("untitled"),
            hit["sessionId"].as_str().unwrap_or("")
        );
        println!(
            "  {}  {DIM}score {:.2}{RESET}",
            truncate(hit["snippet"].as_str().unwrap_or(""), 100),
            hit["score"].as_f64().unwrap_or(0.0)
        );
    }
    println!("{DIM}{} hit(s){RESET}", hits.len());
}

pub fn print_sessions(list: &Value) {
    let groups: Vec<&Value> = list["groups"]
        .as_array()
        .map(|g| g.iter().collect())
        .unwrap_or_default();
    for group in &groups {
        println!(
            "{DIM}▸ {} ({}){RESET}",
            group["name"].as_str().unwrap_or(""),
            group["id"].as_str().unwrap_or("")
        );
    }
    let Some(sessions) = list["sessions"].as_array() else {
        return;
    };
    for session in sessions {
        println!(
            "{}{}{RESET}  {DIM}{}  {} msg · {} tok · {}{RESET}",
            if session["pinned"].as_bool().unwrap_or(false) {
                BOLD
            } else {
                ""
            },
            session["title"].as_str().unwrap_or("untitled"),
            session["id"].as_str().unwrap_or(""),
            num(&session["messageCount"]),
            num(&session["totalTokens"]),
            session["status"].as_str().unwrap_or("idle"),
        );
    }
    println!("{DIM}{} session(s){RESET}", sessions.len());
}

// ---------------------------------------------------------------- helpers

pub fn sparkline(series: &[u64]) -> String {
    let max = series.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return SPARK[0].to_string().repeat(series.len());
    }
    series
        .iter()
        .map(|v| {
            let idx = ((*v as f64 / max as f64) * (SPARK.len() - 1) as f64).round() as usize;
            SPARK[idx.min(SPARK.len() - 1)]
        })
        .collect()
}

/// Thousands separators without a dependency.
pub fn num(value: &Value) -> String {
    let n = value.as_u64().unwrap_or(0);
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn truncate(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        return flat;
    }
    flat.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
