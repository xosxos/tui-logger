//! # Logger with smart widget for the `tui` and `ratatui` crate

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

use std::sync::OnceLock;

static TUI_LOGGER: OnceLock<TuiLogger> = OnceLock::new();

pub fn tracing_subscriber_layer(buffer_size: usize) -> TuiLogger {
    TUI_LOGGER.get_or_init(|| TuiLogger {
        records: CircularBuffer::new(buffer_size),
    })
}

struct ExtLogRecord {
    timestamp: DateTime<Local>,
    level: tracing::Level,
    target: String,
    file: String,
    line: u32,
    msg: String,
}

struct TuiLogger {
    records: Mutex<CircularBuffer<ExtLogRecord>>,
}

// Implement tracing layer
impl<S> tracing_subscriber::Layer<S> for TuiLogger
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = tracing_visitor::ToStringVisitor::default();
        event.record(&mut visitor);
          
        let record = ExtLogRecord {
            timestamp: chrono::Local::now(),
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            file: event.metadata().file().unwrap_or("?").to_string(),
            line: event.metadata().line().unwrap_or(0),
            msg: format_args!("{}", visitor)),
        };
        
        self.records.lock().push(record); 
    }
}


pub struct TuiLoggerWidget<'b> {
    block: Option<Block<'b>>,
    style: Style,
    style_error: Option<Style>,
    style_warn: Option<Style>,
    style_debug: Option<Style>,
    style_trace: Option<Style>,
    style_info: Option<Style>,
    format_separator: char,
    format_timestamp: Option<String>,
}

impl<'b> Default for TuiLoggerWidget<'b> {
    fn default() -> TuiLoggerWidget<'b> {
        TuiLoggerWidget {
            style: Style::default(),
            style_error: None,
            style_warn: None,
            style_debug: None,
            style_trace: None,
            style_info: None,
            format_separator: ':',
            format_timestamp: Some("%H:%M:%S".to_string()),
        }
    }
}

impl<'b> TuiLoggerWidget<'b> {
    fn format_record(&self, evt: &ExtLogRecord) -> String {
        let mut output = String::new();
        
        if let Some(fmt) = self.format_timestamp.as_ref() {
            output.push_str(&format!("{}", evt.timestamp.format(fmt)));
            output.push(self.format_separator);
        }
        
        output.push_str(evt.level.to_string());
        output.push(self.format_separator);
        
        if evt.level == tracing::Level::ERROR {
            output.push_str(&evt.file);
            output.push(self.format_separator);
            output.push_str(&format!("{}", evt.line));
            output.push(self.format_separator);
        }
        
        output
    }
}

impl<'b> Widget for TuiLoggerWidget<'b> {
    fn render(mut self, list_area: Rect, buf: &mut Buffer) {
        buf.set_style(area, self.style);
        
        let indent = 9;
        
        if list_area.width < indent + 4 || list_area.height < 1 {
            return;
        }
        
        let la_left = list_area.left();
        let la_top = list_area.top();
        let la_width = list_area.width as usize;
        let la_height = list_area.height as usize;
        
        let rem_width = la_width - indent as usize;

        // Raw string lines
        let mut lines: Vec<(Option<Style>, u16, String)> = vec![];

        // Get the lock
        let mut tui_lock = TUI_LOGGER.get().unwrap().records.lock();

        // Loop records
        for record in tui_lock.records.rev_iter() {
            let col_style = match record.level {
                tracing::Level::ERROR => self.style_error,
                tracing::Level::WARN => self.style_warn,
                tracing::Level::INFO => self.style_info,
                tracing::Level::DEBUG => self.style_debug,
                tracing::Level::TRACE => self.style_trace,
            };
                
            let mut output = self.format_record(record);
                
            let mut sublines: Vec<&str> = record.msg.lines().rev().collect();
                
            output.push_str(sublines.pop().unwrap());
                
            for subline in sublines {
                lines.push((col_style, indent, subline.to_string()));
            }
                
             lines.push((col_style, 0, output));
                
            if lines.len() == la_height {
                break;
            }
        }

        // Drop the lock
        drop(tui_lock);
        
        // lines is a vector with bottom line at index 0
        // wrapped_lines will be a vector with top line first
        let mut wrapped_lines = CircularBuffer::new(la_height);
        
        while let Some((style, left, line)) = lines.pop() {
            // Word wrap
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

        // Write records to the Widgets buffer
        for (i, (style, left, line)) in wrapped_lines.iter().enumerate() {
            buf.set_string(
                la_left + left,
                la_top + i as u16,
                line,
                line.len(),
                style.unwrap_or(self.style),
            );
        }
    }
}

#[rustfmt::skip]
pub mod tracing_visitor {
    use std::{fmt, error, collections::HashMap};
    use tracing::field;

    #[derive(Default)]
    struct ToStringVisitor<'a>(HashMap<&'a str, String>);

    impl fmt::Display for ToStringVisitor<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.0.iter().try_for_each(|(k, v)| -> fmt::Result { write!(f, " {}: {}", k, v) })
        }
    }
    
    macro_rules! insert_value_as_string_to_visitor(
        () => ( self.0.insert(field.name(), format_args!("{}", value).to_string()); );
    )

    impl<'a> Visit for ToStringVisitor<'a> {
        fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
            insert_value_as_string_to_visitor!()
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            insert_value_as_string_to_visitor!()
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            insert_value_as_string_to_visitor!()
        }

        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            insert_value_as_string_to_visitor!()
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            insert_value_as_string_to_visitor!()
        }

        fn record_error(&mut self, field: &tracing::field::Field, value: &(dyn error::Error + 'static)) {
            insert_value_as_string_to_visitor!()
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
            insert_value_as_string_to_visitor!()
        }
    }
}

#[rustfmt::skip]
mod circular {
    use std::{iter, slice};
    
    pub struct CircularBuffer<T> {
        buffer: Vec<T>,
        next_write_pos: usize,
    }
    
impl<T> CircularBuffer<T> {
    /// Create a new CircularBuffer, which can hold max_depth elements
    pub fn new(max_depth: usize) -> Self<T> {
        Self {
            buffer: Vec::with_capacity(max_depth),
            next_write_pos: 0,
        }
    }    
    /// Elements are pushed until the capacity is reached.
    /// Afterwards the oldest elements will be overwritten.
    pub fn push(&mut self, elem: T) {
        match self.buffer.len() < self.buffer.capacity() {
            true => self.buffer.push(elem),
            false => self.buffer[self.next_write_pos % self.buffer.capacity()] = elem,
        }
        
        self.next_write_pos += 1;
    }
    /// Take out all elements from the buffer, leaving an empty buffer behind
    pub fn take(&mut self) -> Vec<T> {
        let mut consumed = vec![];
        
        match self.buffer.len() < self.buffer.capacity() {
            true => consumed.append(&mut self.buffer),
            false => { 
                let wrap_idx = self.next_write_pos % self.buffer.capacity();
                consumed.append(&mut self.buffer.split_off(wrap_idx));
                consumed.append(&mut self.buffer)
            }
        }
        
        self.next_write_pos = 0;
        consumed
    }
    /// Return an iterator to step through all elements in the sequence,
    /// as these have been pushed (FIFO)
    pub fn iter(&mut self) -> iter::Chain<slice::Iter<T>, slice::Iter<T>> {
        // Check if buffer is completely filled
        match self.next_write_pos < self.buffer.capacity() {
            // If not, then just iterate through it
            true => self.buffer[0..].iter().chain(self.buffer[..0].iter()),
            // If yes, find wrap around index and chain around it
            false => {
                let wrap_idx = self.next_write_pos % self.buffer.capacity();
                self.buffer[wrap_idx..].iter().chain(self.buffer[..wrap_idx].iter())
            }
        }
    }
    /// Return an iterator to step through all elements in the reverse sequence,
    /// as these have been pushed (LIFO)
    pub fn rev_iter(&mut self) -> iter::Chain<iter::Rev<slice::Iter<T>>, iter::Rev<slice::Iter<T>>> {
        // Check if buffer is completely filled
        match self.next_write_pos < self.buffer.capacity() {
            // If not, then just reverse iterate through it
            true => self.buffer[0..].iter().rev().chain(self.buffer[..0].iter().rev()),
            // If yes, find wrap around index and chain around it
            false => {
                let wrap_idx = self.next_write_pos % self.buffer.capacity();
                self.buffer[..wrap_idx].iter().rev().chain(self.buffer[wrap_idx..].iter().rev())
            }
        }
    }
}
}
