//! Translate Neovim's ext_linegrid UI events into Herdr's semantic cells.
use super::wire;
use anyhow::{Result, ensure};
use rmpv::Value;
use std::collections::HashMap;

pub struct Grid {
    pub width: u16,
    pub height: u16,
    cells: Vec<(String, u64)>,
    highlights: HashMap<u64, Value>,
    fg: u32,
    bg: u32,
    cursor: (u16, u16),
    shape: u8,
    modes: Vec<Value>,
    pub ready: bool,
    pub dirty: bool,
}
fn num(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}
fn attr<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_map()?
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
}
impl Grid {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            cells: vec![],
            highlights: HashMap::new(),
            fg: 0,
            bg: 0,
            cursor: (0, 0),
            shape: 2,
            modes: vec![],
            ready: false,
            dirty: false,
        }
    }
    pub fn clear(&mut self, width: u16, height: u16) {
        *self = Self::new();
        self.width = width;
        self.height = height;
        self.cells = vec![(" ".into(), 0); usize::from(width) * usize::from(height)];
        self.ready = true;
        self.dirty = true;
    }
    pub fn redraw(&mut self, events: &[Value]) -> Result<()> {
        for event in events {
            let Some(parts) = event.as_array() else {
                continue;
            };
            let Some(name) = parts.first().and_then(Value::as_str) else {
                continue;
            };
            for args in &parts[1..] {
                let Some(a) = args.as_array() else { continue };
                match name {
                    "grid_resize" if a.len() == 3 && num(&a[0]) == 1 => {
                        let (w, h) = (num(&a[1]), num(&a[2]));
                        ensure!(
                            w > 0 && h > 0 && w * h <= 100_000,
                            "invalid Neovim grid dimensions"
                        );
                        self.width = w as u16;
                        self.height = h as u16;
                        self.cells = vec![(" ".into(), 0); (w * h) as usize];
                    }
                    "grid_clear" if a.first().map(num) == Some(1) => {
                        self.cells.fill((" ".into(), 0))
                    }
                    "grid_line" if a.len() >= 4 && num(&a[0]) == 1 => {
                        let row = num(&a[1]) as usize;
                        let mut col = num(&a[2]) as usize;
                        let mut hl = 0;
                        if let Some(cells) = a[3].as_array() {
                            for cell in cells {
                                let Some(c) = cell.as_array() else { continue };
                                if c.len() > 1 {
                                    hl = num(&c[1]);
                                }
                                let repeat =
                                    c.get(2).map(num).unwrap_or(1).min(u64::from(self.width));
                                for _ in 0..repeat {
                                    if row < self.height as usize && col < self.width as usize {
                                        self.cells[row * self.width as usize + col] =
                                            (c[0].as_str().unwrap_or(" ").into(), hl);
                                    }
                                    col += 1;
                                }
                            }
                        }
                    }
                    "grid_scroll" if a.len() == 7 && num(&a[0]) == 1 => {
                        let (top, bot, left, right) = (
                            num(&a[1]) as usize,
                            num(&a[2]) as usize,
                            num(&a[3]) as usize,
                            num(&a[4]) as usize,
                        );
                        let (dr, dc) = (a[5].as_i64().unwrap_or(0), a[6].as_i64().unwrap_or(0));
                        let old = self.cells.clone();
                        for row in top..bot.min(self.height as usize) {
                            for col in left..right.min(self.width as usize) {
                                let (r, c) = (row as i64 + dr, col as i64 + dc);
                                self.cells[row * self.width as usize + col] = if r >= top as i64
                                    && r < bot as i64
                                    && c >= left as i64
                                    && c < right as i64
                                {
                                    old.get(r as usize * self.width as usize + c as usize)
                                        .cloned()
                                        .unwrap_or((" ".into(), 0))
                                } else {
                                    (" ".into(), 0)
                                };
                            }
                        }
                    }
                    "grid_cursor_goto" if a.len() == 3 && num(&a[0]) == 1 => {
                        self.cursor = (num(&a[2]) as u16, num(&a[1]) as u16)
                    }
                    "hl_attr_define" if a.len() >= 2 => {
                        self.highlights.insert(num(&a[0]), a[1].clone());
                    }
                    "default_colors_set" if a.len() >= 2 => {
                        self.fg = a[0].as_u64().map(|n| 0x02000000 | n as u32).unwrap_or(0);
                        self.bg = a[1].as_u64().map(|n| 0x02000000 | n as u32).unwrap_or(0);
                    }
                    "mode_info_set" if a.len() >= 2 => {
                        self.modes = a[1].as_array().cloned().unwrap_or_default()
                    }
                    "mode_change" if a.len() >= 2 => {
                        self.shape = self
                            .modes
                            .get(num(&a[1]) as usize)
                            .and_then(|m| attr(m, "cursor_shape"))
                            .and_then(Value::as_str)
                            .map(|s| match s {
                                "vertical" => 6,
                                "horizontal" => 4,
                                _ => 2,
                            })
                            .unwrap_or(2);
                    }
                    "flush" => {
                        self.ready = true;
                        self.dirty = true;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
    pub fn cells(&self) -> Vec<wire::Cell> {
        self.cells
            .iter()
            .map(|(text, id)| {
                let hl = self.highlights.get(id);
                let color = |name, default| {
                    hl.and_then(|h| attr(h, name))
                        .and_then(Value::as_u64)
                        .map(|n| 0x02000000 | n as u32)
                        .unwrap_or(default)
                };
                let mut fg = color("foreground", self.fg);
                let mut bg = color("background", self.bg);
                let yes = |name| {
                    hl.and_then(|h| attr(h, name))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                };
                if yes("reverse") {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let mut modifier = 0;
                for (name, bit) in [
                    ("bold", 1),
                    ("italic", 4),
                    ("underline", 8),
                    ("undercurl", 8),
                    ("strikethrough", 256),
                ] {
                    if yes(name) {
                        modifier |= bit;
                    }
                }
                wire::Cell {
                    symbol: if text.is_empty() {
                        " ".into()
                    } else {
                        text.clone()
                    },
                    fg,
                    bg,
                    modifier,
                    skip: text.is_empty(),
                    hyperlink: None,
                }
            })
            .collect()
    }
    pub fn cursor(&self) -> wire::Cursor {
        wire::Cursor {
            x: self.cursor.0,
            y: self.cursor.1,
            visible: true,
            shape: self.shape,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn values(value: serde_json::Value) -> Value {
        match value {
            serde_json::Value::Array(a) => Value::Array(a.into_iter().map(values).collect()),
            serde_json::Value::Object(o) => {
                Value::Map(o.into_iter().map(|(k, v)| (k.into(), values(v))).collect())
            }
            serde_json::Value::String(s) => s.into(),
            serde_json::Value::Number(n) => n.as_i64().unwrap().into(),
            serde_json::Value::Bool(b) => b.into(),
            _ => Value::Nil,
        }
    }
    #[test]
    fn highlights_wide_cells_repeats_scroll_and_cursor() {
        let mut grid = Grid::new();
        let events = values(serde_json::json!([
            ["grid_resize",[1,4,2]], ["default_colors_set",[16777215,0,0,0,0]],
            ["hl_attr_define",[1,{"foreground":16711680,"bold":true,"reverse":true}]],
            ["grid_line",[1,0,0,[["界",1],[""],["x",0,2]],false]],
            ["grid_line",[1,1,0,[["z",1,4]],false]],
            ["grid_cursor_goto",[1,1,2]], ["flush",[]]
        ]));
        grid.redraw(events.as_array().unwrap()).unwrap();
        assert!(grid.ready && grid.dirty);
        let cells = grid.cells();
        assert_eq!(cells[0].symbol, "界");
        assert!(cells[1].skip);
        assert_eq!(cells[0].fg, 0x02000000);
        assert_eq!(cells[0].bg, 0x02ff0000);
        assert_eq!(cells[0].modifier, 1);
        assert_eq!(cells[3].symbol, "x");
        assert_eq!(grid.cursor().x, 2);
        assert_eq!(grid.cursor().y, 1);
        let scroll = values(serde_json::json!([
            ["grid_scroll", [1, 0, 2, 0, 4, 1, 0]],
            ["flush", []]
        ]));
        grid.redraw(scroll.as_array().unwrap()).unwrap();
        assert_eq!(grid.cells()[0].symbol, "z");
        assert_eq!(grid.cells()[4].symbol, " ");
    }
    #[test]
    fn rejects_unbounded_grid_allocations() {
        let mut grid = Grid::new();
        let event = values(serde_json::json!([["grid_resize", [1, 10000, 10000]]]));
        assert!(grid.redraw(event.as_array().unwrap()).is_err());
    }
}
