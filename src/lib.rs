//! # Logger with smart widget for the `tui` and `ratatui` crate

#![cfg_attr(docsrs, feature(doc_cfg))]
#[macro_use]
extern crate lazy_static;

use std::collections::hash_map::Iter;
use std::collections::hash_map::Keys;
use std::collections::HashMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::mem;
use std::sync::Arc;
use std::thread;

use chrono::{DateTime, Local};
use parking_lot::Mutex;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Widget},
};

pub use crate::circular::CircularBuffer;
pub use crate::tracing_subscriber::TuiTracingSubscriberLayer;

struct ExtLogRecord {
    timestamp: DateTime<Local>,
    level: Level,
    target: String,
    file: String,
    line: u32,
    msg: String,
}

struct TuiLoggerInner {
    events: CircularBuffer<ExtLogRecord>,
    total_events: usize,
    dump: Option<File>,
    default: LevelFilter,
}
struct TuiLogger {
    inner: Mutex<TuiLoggerInner>,
}

lazy_static! {
    static ref TUI_LOGGER: TuiLogger = {
        let tli = TuiLoggerInner {
            events: CircularBuffer::new(10000),
            total_events: 0,
            dump: None,
            default: LevelFilter::Info,
        };
        TuiLogger {
            inner: Mutex::new(tli),
        }
    };
}

// Lots of boilerplate code, so that init_logger can return two error types...
#[derive(Debug)]
pub enum TuiLoggerError {
    SetLoggerError(SetLoggerError),
    ThreadError(std::io::Error),
}
impl std::error::Error for TuiLoggerError {
    fn description(&self) -> &str {
        match self {
            TuiLoggerError::SetLoggerError(_) => "SetLoggerError",
            TuiLoggerError::ThreadError(_) => "ThreadError",
        }
    }
    fn cause(&self) -> Option<&dyn std::error::Error> {
        match self {
            TuiLoggerError::SetLoggerError(_) => None,
            TuiLoggerError::ThreadError(err) => Some(err),
        }
    }
}
impl std::fmt::Display for TuiLoggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TuiLoggerError::SetLoggerError(err) => write!(f, "SetLoggerError({})", err),
            TuiLoggerError::ThreadError(err) => write!(f, "ThreadError({})", err),
        }
    }
}


pub fn tracing_subscriber_layer() -> TuiTracingSubscriberLayer {
    TuiTracingSubscriberLayer
}

/// Set the depth of the circular buffer in order to avoid message loss.
/// This will delete all existing messages in the circular buffer.
pub fn set_buffer_depth(depth: usize) {
    TUI_LOGGER.inner.lock().events = CircularBuffer::new(depth);
}

/// Set default levelfilter for unknown targets of the logger
pub fn set_default_level(levelfilter: LevelFilter) {
    TUI_LOGGER.inner.lock().default = levelfilter;
}

impl TuiLogger {
    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.raw_log(record)
        }
    }
    
    fn raw_log(&self, record: &Record) {
        let log_entry = ExtLogRecord {
            timestamp: chrono::Local::now(),
            level: record.level(),
            target: record.target().to_string(),
            file: record.file().unwrap_or("?").to_string(),
            line: record.line().unwrap_or(0),
            msg: format!("{}", record.args()),
        };
        let mut events_lock = self.inner.lock();
        events_lock.events.push(log_entry);
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum TuiWidgetEvent {
    SpaceKey,
    UpKey,
    DownKey,
    LeftKey,
    RightKey,
    PlusKey,
    MinusKey,
    HideKey,
    FocusKey,
    PrevPageKey,
    NextPageKey,
    EscapeKey,
}

#[derive(Default)]
struct TuiWidgetInnerState {
    nr_items: usize,
    selected: usize,
    opt_timestamp_bottom: Option<DateTime<Local>>,
    opt_timestamp_next_page: Option<DateTime<Local>>,
    opt_timestamp_prev_page: Option<DateTime<Local>>,
    opt_selected_target: Option<String>,
    opt_selected_visibility_more: Option<LevelFilter>,
    opt_selected_visibility_less: Option<LevelFilter>,
    opt_selected_recording_more: Option<LevelFilter>,
    opt_selected_recording_less: Option<LevelFilter>,
    offset: usize,
    hide_off: bool,
    hide_target: bool,
    focus_selected: bool,
}
impl TuiWidgetInnerState {
    pub fn new() -> TuiWidgetInnerState {
        TuiWidgetInnerState::default()
    }
    fn transition(&mut self, event: TuiWidgetEvent) {
        use TuiWidgetEvent::*;
        match event {
            SpaceKey => {
                self.hide_off ^= true;
            }
            HideKey => {
                self.hide_target ^= true;
            }
            FocusKey => {
                self.focus_selected ^= true;
            }
            UpKey => {
                if !self.hide_target && self.selected > 0 {
                    self.selected -= 1;
                }
            }
            DownKey => {
                if !self.hide_target && self.selected + 1 < self.nr_items {
                    self.selected += 1;
                }
            }
            LeftKey => {
                if let Some(selected_target) = self.opt_selected_target.take() {
                    if let Some(selected_visibility_less) = self.opt_selected_visibility_less.take()
                    {
                        self.config.set(&selected_target, selected_visibility_less);
                    }
                }
            }
            RightKey => {
                if let Some(selected_target) = self.opt_selected_target.take() {
                    if let Some(selected_visibility_more) = self.opt_selected_visibility_more.take()
                    {
                        self.config.set(&selected_target, selected_visibility_more);
                    }
                }
            }
            PlusKey => {
                if let Some(selected_target) = self.opt_selected_target.take() {
                    if let Some(selected_recording_more) = self.opt_selected_recording_more.take() {
                        set_level_for_target(&selected_target, selected_recording_more);
                    }
                }
            }
            MinusKey => {
                if let Some(selected_target) = self.opt_selected_target.take() {
                    if let Some(selected_recording_less) = self.opt_selected_recording_less.take() {
                        set_level_for_target(&selected_target, selected_recording_less);
                    }
                }
            }
            PrevPageKey => self.opt_timestamp_bottom = self.opt_timestamp_prev_page,
            NextPageKey => self.opt_timestamp_bottom = self.opt_timestamp_next_page,
            EscapeKey => self.opt_timestamp_bottom = None,
        }
    }
}

/// This struct contains the shared state of a TuiLoggerWidget and a TuiLoggerTargetWidget.
#[derive(Default)]
pub struct TuiWidgetState {
    inner: Arc<Mutex<TuiWidgetInnerState>>,
}
impl TuiWidgetState {
    /// Create a new TuiWidgetState
    pub fn new() -> TuiWidgetState {
        TuiWidgetState {
            inner: Arc::new(Mutex::new(TuiWidgetInnerState::new())),
        }
    }
    pub fn set_default_display_level(self, levelfilter: LevelFilter) -> TuiWidgetState {
        self.inner.lock().config.default_display_level = Some(levelfilter);
        self
    }
    pub fn set_level_for_target(self, target: &str, levelfilter: LevelFilter) -> TuiWidgetState {
        self.inner.lock().config.set(target, levelfilter);
        self
    }
    pub fn transition(&mut self, event: TuiWidgetEvent) {
        self.inner.lock().transition(event);
    }
}

/// The TuiLoggerWidget shows the logging messages in an endless scrolling view.
/// It is controlled by a TuiWidgetState for selected events.
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub enum TuiLoggerLevelOutput {
    Abbreviated,
    Long,
}

pub struct TuiLoggerWidget<'b> {
    block: Option<Block<'b>>,
    /// Base style of the widget
    style: Style,
    /// Level based style
    style_error: Option<Style>,
    style_warn: Option<Style>,
    style_debug: Option<Style>,
    style_trace: Option<Style>,
    style_info: Option<Style>,
    format_separator: char,
    format_timestamp: Option<String>,
    format_output_level: Option<TuiLoggerLevelOutput>,
    format_output_target: bool,
    format_output_file: bool,
    format_output_line: bool,
    state: Arc<Mutex<TuiWidgetInnerState>>,
}
impl<'b> Default for TuiLoggerWidget<'b> {
    fn default() -> TuiLoggerWidget<'b> {
        //TUI_LOGGER.move_events();
        TuiLoggerWidget {
            block: None,
            style: Default::default(),
            style_error: None,
            style_warn: None,
            style_debug: None,
            style_trace: None,
            style_info: None,
            format_separator: ':',
            format_timestamp: Some("%H:%M:%S".to_string()),
            format_output_level: Some(TuiLoggerLevelOutput::Long),
            format_output_target: true,
            format_output_file: true,
            format_output_line: true,
            state: Arc::new(Mutex::new(TuiWidgetInnerState::new())),
        }
    }
}
impl<'b> TuiLoggerWidget<'b> {
    pub fn block(mut self, block: Block<'b>) -> Self {
        self.block = Some(block);
        self
    }
    fn opt_style(mut self, style: Option<Style>) -> Self {
        if let Some(s) = style {
            self.style = s;
        }
        self
    }
    fn opt_style_error(mut self, style: Option<Style>) -> Self {
        if style.is_some() {
            self.style_error = style;
        }
        self
    }
    fn opt_style_warn(mut self, style: Option<Style>) -> Self {
        if style.is_some() {
            self.style_warn = style;
        }
        self
    }
    fn opt_style_info(mut self, style: Option<Style>) -> Self {
        if style.is_some() {
            self.style_info = style;
        }
        self
    }
    fn opt_style_trace(mut self, style: Option<Style>) -> Self {
        if style.is_some() {
            self.style_trace = style;
        }
        self
    }
    fn opt_style_debug(mut self, style: Option<Style>) -> Self {
        if style.is_some() {
            self.style_debug = style;
        }
        self
    }
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
    pub fn style_error(mut self, style: Style) -> Self {
        self.style_error = Some(style);
        self
    }
    pub fn style_warn(mut self, style: Style) -> Self {
        self.style_warn = Some(style);
        self
    }
    pub fn style_info(mut self, style: Style) -> Self {
        self.style_info = Some(style);
        self
    }
    pub fn style_trace(mut self, style: Style) -> Self {
        self.style_trace = Some(style);
        self
    }
    pub fn style_debug(mut self, style: Style) -> Self {
        self.style_debug = Some(style);
        self
    }
    fn opt_output_separator(mut self, opt_sep: Option<char>) -> Self {
        if let Some(ch) = opt_sep {
            self.format_separator = ch;
        }
        self
    }
    /// Separator character between field.
    /// Default is ':'
    pub fn output_separator(mut self, sep: char) -> Self {
        self.format_separator = sep;
        self
    }
    fn opt_output_timestamp(mut self, opt_fmt: Option<Option<String>>) -> Self {
        if let Some(fmt) = opt_fmt {
            self.format_timestamp = fmt;
        }
        self
    }
    /// The format string can be defined as described in
    /// <https://docs.rs/chrono/0.4.19/chrono/format/strftime/index.html>
    ///
    /// If called with None, timestamp is not included in output.
    ///
    /// Default is %H:%M:%S
    pub fn output_timestamp(mut self, fmt: Option<String>) -> Self {
        self.format_timestamp = fmt;
        self
    }
    fn opt_output_level(mut self, opt_fmt: Option<Option<TuiLoggerLevelOutput>>) -> Self {
        if let Some(fmt) = opt_fmt {
            self.format_output_level = fmt;
        }
        self
    }
    /// Possible values are
    /// - TuiLoggerLevelOutput::Long        => DEBUG/TRACE/...
    /// - TuiLoggerLevelOutput::Abbreviated => D/T/...
    ///
    /// If called with None, level is not included in output.
    ///
    /// Default is Long
    pub fn output_level(mut self, level: Option<TuiLoggerLevelOutput>) -> Self {
        self.format_output_level = level;
        self
    }
    fn opt_output_target(mut self, opt_enabled: Option<bool>) -> Self {
        if let Some(enabled) = opt_enabled {
            self.format_output_target = enabled;
        }
        self
    }
    /// Enables output of target field of event
    ///
    /// Default is true
    pub fn output_target(mut self, enabled: bool) -> Self {
        self.format_output_target = enabled;
        self
    }
    fn opt_output_file(mut self, opt_enabled: Option<bool>) -> Self {
        if let Some(enabled) = opt_enabled {
            self.format_output_file = enabled;
        }
        self
    }
    /// Enables output of file field of event
    ///
    /// Default is true
    pub fn output_file(mut self, enabled: bool) -> Self {
        self.format_output_file = enabled;
        self
    }
    fn opt_output_line(mut self, opt_enabled: Option<bool>) -> Self {
        if let Some(enabled) = opt_enabled {
            self.format_output_line = enabled;
        }
        self
    }
    /// Enables output of line field of event
    ///
    /// Default is true
    pub fn output_line(mut self, enabled: bool) -> Self {
        self.format_output_line = enabled;
        self
    }
    fn inner_state(mut self, state: Arc<Mutex<TuiWidgetInnerState>>) -> Self {
        self.state = state;
        self
    }
    pub fn state(mut self, state: &TuiWidgetState) -> Self {
        self.state = state.inner.clone();
        self
    }
    fn format_event(&self, evt: &ExtLogRecord) -> (String, Option<Style>) {
        let mut output = String::new();
        let (col_style, lev_long, lev_abbr, with_loc) = match evt.level {
            log::Level::Error => (self.style_error, "ERROR", "E", true),
            log::Level::Warn => (self.style_warn, "WARN ", "W", true),
            log::Level::Info => (self.style_info, "INFO ", "I", false),
            log::Level::Debug => (self.style_debug, "DEBUG", "D", true),
            log::Level::Trace => (self.style_trace, "TRACE", "T", true),
        };
        if let Some(fmt) = self.format_timestamp.as_ref() {
            output.push_str(&format!("{}", evt.timestamp.format(fmt)));
            output.push(self.format_separator);
        }
        match &self.format_output_level {
            None => {}
            Some(TuiLoggerLevelOutput::Abbreviated) => {
                output.push_str(lev_abbr);
                output.push(self.format_separator);
            }
            Some(TuiLoggerLevelOutput::Long) => {
                output.push_str(lev_long);
                output.push(self.format_separator);
            }
        }
        if self.format_output_target {
            output.push_str(&evt.target);
            output.push(self.format_separator);
        }
        if with_loc {
            if self.format_output_file {
                output.push_str(&evt.file);
                output.push(self.format_separator);
            }
            if self.format_output_line {
                output.push_str(&format!("{}", evt.line));
                output.push(self.format_separator);
            }
        }
        (output, col_style)
    }
}
impl<'b> Widget for TuiLoggerWidget<'b> {
    fn render(mut self, area: Rect, buf: &mut Buffer) {
        buf.set_style(area, self.style);
        let list_area = match self.block.take() {
            Some(b) => {
                let inner_area = b.inner(area);
                b.render(area, buf);
                inner_area
            }
            None => area,
        };
        let indent = 9;
        if list_area.width < indent + 4 || list_area.height < 1 {
            return;
        }

        let mut state = self.state.lock();
        let la_height = list_area.height as usize;
        let mut lines: Vec<(Option<Style>, u16, String)> = vec![];
        {
            state.opt_timestamp_next_page = None;
            let opt_timestamp_bottom = state.opt_timestamp_bottom;
            let mut opt_timestamp_prev_page = None;
            let mut tui_lock = TUI_LOGGER.inner.lock();
            let mut circular = CircularBuffer::new(10); // MAGIC constant
            for evt in tui_lock.events.rev_iter() {
                if let Some(level) = state.config.get(&evt.target) {
                    if level < evt.level {
                        continue;
                    }
                } else if let Some(level) = state.config.default_display_level {
                    if level < evt.level {
                        continue;
                    }
                }
                if state.focus_selected {
                    if let Some(target) = state.opt_selected_target.as_ref() {
                        if target != &evt.target {
                            continue;
                        }
                    }
                }
                // Here all filters have been applied,
                // So check, if user is paging through history
                if let Some(timestamp) = opt_timestamp_bottom.as_ref() {
                    if *timestamp < evt.timestamp {
                        circular.push(evt.timestamp);
                        continue;
                    }
                }
                if !circular.is_empty() {
                    state.opt_timestamp_next_page = circular.take().first().cloned();
                }
                let (mut output, col_style) = self.format_event(evt);
                let mut sublines: Vec<&str> = evt.msg.lines().rev().collect();
                output.push_str(sublines.pop().unwrap());
                for subline in sublines {
                    lines.push((col_style, indent, subline.to_string()));
                }
                lines.push((col_style, 0, output));
                if lines.len() == la_height {
                    break;
                }
                if opt_timestamp_prev_page.is_none() && lines.len() >= la_height / 2 {
                    opt_timestamp_prev_page = Some(evt.timestamp);
                }
            }
            state.opt_timestamp_prev_page = opt_timestamp_prev_page.or(state.opt_timestamp_bottom);
        }
        let la_left = list_area.left();
        let la_top = list_area.top();
        let la_width = list_area.width as usize;

        // lines is a vector with bottom line at index 0
        // wrapped_lines will be a vector with top line first
        let mut wrapped_lines = CircularBuffer::new(la_height);
        let rem_width = la_width - indent as usize;
        while let Some((style, left, line)) = lines.pop() {
            if line.chars().count() > la_width {
                wrapped_lines.push((style, left, line.chars().take(la_width).collect()));
                let mut remain: String = line.chars().skip(la_width).collect();
                while remain.chars().count() > rem_width {
                    let remove: String = remain.chars().take(rem_width).collect();
                    wrapped_lines.push((style, indent, remove));
                    remain = remain.chars().skip(rem_width).collect();
                }
                wrapped_lines.push((style, indent, remain.to_owned()));
            } else {
                wrapped_lines.push((style, left, line));
            }
        }

        let offset: u16 = if state.opt_timestamp_bottom.is_none() {
            0
        } else {
            let lines_cnt = wrapped_lines.len();
            (la_height - lines_cnt) as u16
        };

        for (i, (sty, left, l)) in wrapped_lines.iter().enumerate() {
            buf.set_stringn(
                la_left + left,
                la_top + i as u16 + offset,
                l,
                l.len(),
                sty.unwrap_or(self.style),
            );
        }
    }
}

pub mod tracing_subscriber {
    //! `tracing-subscriber` support for `tui-logger`

use super::TUI_LOGGER;
use log::{self, Log, Record};
use std::collections::HashMap;
use std::fmt;
use tracing_subscriber::Layer;

#[derive(Default)]
struct ToStringVisitor<'a>(HashMap<&'a str, String>);

impl fmt::Display for ToStringVisitor<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|(k, v)| -> fmt::Result { write!(f, " {}: {}", k, v) })
    }
}

impl<'a> tracing::field::Visit for ToStringVisitor<'a> {
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.0
            .insert(field.name(), format_args!("{}", value).to_string());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0
            .insert(field.name(), format_args!("{}", value).to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0
            .insert(field.name(), format_args!("{}", value).to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0
            .insert(field.name(), format_args!("{}", value).to_string());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0
            .insert(field.name(), format_args!("{}", value).to_string());
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.0
            .insert(field.name(), format_args!("{}", value).to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name(), format_args!("{:?}", value).to_string());
    }
}

pub struct TuiTracingSubscriberLayer;

impl<S> Layer<S> for TuiTracingSubscriberLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = ToStringVisitor::default();
        event.record(&mut visitor);

        let level = match *event.metadata().level() {
            tracing::Level::ERROR => log::Level::Error,
            tracing::Level::WARN => log::Level::Warn,
            tracing::Level::INFO => log::Level::Info,
            tracing::Level::DEBUG => log::Level::Debug,
            tracing::Level::TRACE => log::Level::Trace,
        };

        TUI_LOGGER.log(
            &Record::builder()
                .args(format_args!("{}", visitor))
                .level(level)
                .target(event.metadata().target())
                .file(event.metadata().file())
                .line(event.metadata().line())
                .module_path(event.metadata().module_path())
                .build(),
        );
    }
}
}

mod circular {
    use std::iter;
    
    pub struct CircularBuffer<T> {
    buffer: Vec<T>,
    next_write_pos: usize,
}
#[allow(dead_code)]
impl<T> CircularBuffer<T> {
    /// Create a new CircularBuffer, which can hold max_depth elements
    pub fn new(max_depth: usize) -> CircularBuffer<T> {
        CircularBuffer {
            buffer: Vec::with_capacity(max_depth),
            next_write_pos: 0,
        }
    }
    /// Return the number of elements present in the buffer
    pub fn len(&self) -> usize {
        self.buffer.len()
    }
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }
    /// Push a new element into the buffer.
    /// Until the capacity is reached, elements are pushed.
    /// Afterwards the oldest elements will be overwritten.
    pub fn push(&mut self, elem: T) {
        let max_depth = self.buffer.capacity();
        if self.buffer.len() < max_depth {
            self.buffer.push(elem);
        } else {
            self.buffer[self.next_write_pos % max_depth] = elem;
        }
        self.next_write_pos += 1;
    }
    /// Take out all elements from the buffer, leaving an empty buffer behind
    pub fn take(&mut self) -> Vec<T> {
        let mut consumed = vec![];
        let max_depth = self.buffer.capacity();
        if self.buffer.len() < max_depth {
            consumed.append(&mut self.buffer);
        } else {
            let pos = self.next_write_pos % max_depth;
            let mut xvec = self.buffer.split_off(pos);
            consumed.append(&mut xvec);
            consumed.append(&mut self.buffer)
        }
        self.next_write_pos = 0;
        consumed
    }
    /// Total number of elements pushed into the buffer.
    pub fn total_elements(&self) -> usize {
        self.next_write_pos
    }
    /// If has_wrapped() is true, then elements have been overwritten
    pub fn has_wrapped(&self) -> bool {
        self.next_write_pos > self.buffer.capacity()
    }
    /// Return an iterator to step through all elements in the sequence,
    /// as these have been pushed (FIFO)
    pub fn iter(&mut self) -> iter::Chain<std::slice::Iter<T>, std::slice::Iter<T>> {
        let max_depth = self.buffer.capacity();
        if self.next_write_pos <= max_depth {
            // If buffer is not completely filled, then just iterate through it
            self.buffer.iter().chain(self.buffer[..0].iter())
        } else {
            let wrap = self.next_write_pos % max_depth;
            let it_end = self.buffer[..wrap].iter();
            let it_start = self.buffer[wrap..].iter();
            it_start.chain(it_end)
        }
    }
    /// Return an iterator to step through all elements in the reverse sequence,
    /// as these have been pushed (LIFO)
    pub fn rev_iter(
        &mut self,
    ) -> iter::Chain<std::iter::Rev<std::slice::Iter<T>>, std::iter::Rev<std::slice::Iter<T>>> {
        let max_depth = self.buffer.capacity();
        if self.next_write_pos <= max_depth {
            // If buffer is not completely filled, then just iterate through it
            self.buffer
                .iter()
                .rev()
                .chain(self.buffer[..0].iter().rev())
        } else {
            let wrap = self.next_write_pos % max_depth;
            let it_end = self.buffer[..wrap].iter().rev();
            let it_start = self.buffer[wrap..].iter().rev();
            it_end.chain(it_start)
        }
    }
}
}
