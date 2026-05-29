//! Like time sink, this is mostly LLM coded. It does work as a proof of
//! concept, but it needs to be properly reviewed.
use std::cell::RefCell;
use std::collections::VecDeque;

use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, Event, HtmlCanvasElement};

use crate::mainthread::get_element;
use rustradio_ui::FloatPduStream;

const ID_SPECTRUM_CANVAS: &str = "spectrum-graph";
const ID_WATERFALL_CANVAS: &str = "waterfall-graph";
const MAX_WATERFALL_FRAMES: usize = 40;
const WATERFALL_MIN_DB: f32 = -120.0;
const WATERFALL_MAX_DB: f32 = 0.0;
const AXIS_MARGIN_LEFT: f64 = 54.0;
const AXIS_MARGIN_RIGHT: f64 = 14.0;
const AXIS_MARGIN_TOP: f64 = 12.0;
const AXIS_MARGIN_BOTTOM: f64 = 32.0;
const AXIS_TICK_COUNT: usize = 5;

thread_local! {
    static SPECTRUM_STATE: RefCell<Option<SpectrumState>> = const { RefCell::new(None) };
}

struct SpectrumState {
    latest: Option<FloatPduStream>,
    history: VecDeque<Vec<f32>>,
    sample_rate: f32,
    waterfall_width: u32,
    waterfall_height: u32,
    waterfall_initialized: bool,
}

impl SpectrumState {
    fn new() -> Self {
        Self {
            latest: None,
            history: VecDeque::new(),
            sample_rate: 1.0,
            waterfall_width: 0,
            waterfall_height: 0,
            waterfall_initialized: false,
        }
    }

    fn set_latest(&mut self, frames: Vec<FloatPduStream>) {
        for frame in frames {
            if !frame.samples.is_empty() {
                self.sample_rate = frame.sample_rate;
                self.history.push_back(frame.samples.clone());
                while self.history.len() > MAX_WATERFALL_FRAMES {
                    self.history.pop_front();
                }
            }
            self.latest = Some(frame);
        }
    }
}

fn with_spectrum_state<T>(f: impl FnOnce(&mut SpectrumState) -> T) -> T {
    SPECTRUM_STATE.with(|cell| {
        let mut opt = cell.borrow_mut();
        let state = opt.get_or_insert_with(SpectrumState::new);
        f(state)
    })
}

pub(crate) fn setup_graph_ui() -> Result<(), JsValue> {
    let handler = Closure::<dyn FnMut(Event)>::new(move |_e: Event| {
        let _ = draw_all();
    });
    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
    window.add_event_listener_with_callback("resize", handler.as_ref().unchecked_ref())?;
    handler.forget();

    draw_all()
}

fn draw_all() -> Result<(), JsValue> {
    draw_graph()?;
    draw_waterfall()
}

fn resize_canvas_to_display_size(canvas: &HtmlCanvasElement) -> Result<(f64, f64), JsValue> {
    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
    let dpr = window.device_pixel_ratio();
    let display_width = f64::from(canvas.client_width());
    let display_height = f64::from(canvas.client_height());
    if display_width > 0.0 && display_height > 0.0 {
        let width = (display_width * dpr).round().max(1.0) as u32;
        let height = (display_height * dpr).round().max(1.0) as u32;
        if canvas.width() != width || canvas.height() != height {
            canvas.set_width(width);
            canvas.set_height(height);
        }
    }
    Ok((f64::from(canvas.width()), f64::from(canvas.height())))
}

fn draw_graph() -> Result<(), JsValue> {
    let canvas = get_element(ID_SPECTRUM_CANVAS)?.dyn_into::<HtmlCanvasElement>()?;
    let (width, height) = resize_canvas_to_display_size(&canvas)?;
    let ctx = canvas
        .get_context("2d")?
        .ok_or(JsValue::from_str("no 2d context"))?
        .dyn_into::<CanvasRenderingContext2d>()?;

    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
    let is_dark = window
        .match_media("(prefers-color-scheme: dark)")?
        .is_some_and(|m| m.matches());
    let bg = if is_dark { "#0b0b0b" } else { "#ffffff" };
    let axis = if is_dark { "#666" } else { "#888" };
    let grid = if is_dark { "#242424" } else { "#e7e7e7" };
    let text = if is_dark { "#ddd" } else { "#222" };
    let trace = if is_dark { "#7fcdbb" } else { "#2b8cbe" };

    ctx.set_fill_style_str(bg);
    ctx.fill_rect(0.0, 0.0, width, height);
    ctx.set_stroke_style_str(axis);
    ctx.stroke_rect(0.5, 0.5, (width - 1.0).max(0.0), (height - 1.0).max(0.0));

    SPECTRUM_STATE.with(|cell| -> Result<(), JsValue> {
        let mut opt = cell.borrow_mut();
        let state = opt.get_or_insert_with(SpectrumState::new);
        let Some(frame) = &state.latest else {
            ctx.set_fill_style_str(text);
            ctx.set_font("12px sans-serif");
            ctx.fill_text("Waiting for spectrum data...", 12.0, 20.0)?;
            return Ok(());
        };
        if frame.samples.is_empty() {
            return Ok(());
        }

        let plot_left = AXIS_MARGIN_LEFT.min((width - 1.0).max(0.0));
        let plot_top = AXIS_MARGIN_TOP.min((height - 1.0).max(0.0));
        let plot_width = (width - AXIS_MARGIN_LEFT - AXIS_MARGIN_RIGHT).max(1.0);
        let plot_height = (height - AXIS_MARGIN_TOP - AXIS_MARGIN_BOTTOM).max(1.0);

        let (mut y_min, mut y_max) = data_range(&frame.samples).unwrap_or((-120.0, 0.0));
        if (y_max - y_min).abs() < f32::EPSILON {
            y_min -= 1.0;
            y_max += 1.0;
        }
        let padding = ((y_max - y_min) * 0.08).max(1.0);
        y_min -= padding;
        y_max += padding;

        draw_axes(
            &ctx,
            grid,
            axis,
            text,
            plot_left,
            plot_top,
            plot_width,
            plot_height,
            frame.sample_rate,
            f64::from(y_min),
            f64::from(y_max),
        )?;

        let y_range = f64::from(y_max - y_min).max(1.0e-9);
        let len = frame.samples.len();
        let half = len / 2;
        let denom = (len.saturating_sub(1)).max(1) as f64;

        ctx.set_stroke_style_str(trace);
        ctx.set_line_width(1.25);
        ctx.begin_path();
        for i in 0..len {
            let bin = frame.samples[(i + half) % len];
            let x = plot_left + (i as f64 / denom) * plot_width;
            let y = plot_top + plot_height
                - ((f64::from(bin) - f64::from(y_min)) / y_range) * plot_height;
            if i == 0 {
                ctx.move_to(x, y);
            } else {
                ctx.line_to(x, y);
            }
        }
        ctx.stroke();

        Ok(())
    })?;

    Ok(())
}

fn draw_waterfall() -> Result<(), JsValue> {
    let canvas = get_element(ID_WATERFALL_CANVAS)?.dyn_into::<HtmlCanvasElement>()?;
    let (width, height) = resize_canvas_to_display_size(&canvas)?;
    let ctx = canvas
        .get_context("2d")?
        .ok_or(JsValue::from_str("no 2d context"))?
        .dyn_into::<CanvasRenderingContext2d>()?;

    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
    let is_dark = window
        .match_media("(prefers-color-scheme: dark)")?
        .is_some_and(|m| m.matches());
    let bg = if is_dark { "#0b0b0b" } else { "#ffffff" };
    let axis = if is_dark { "#666" } else { "#888" };
    let grid = if is_dark { "#242424" } else { "#e7e7e7" };
    let text = if is_dark { "#ddd" } else { "#222" };

    SPECTRUM_STATE.with(|cell| -> Result<(), JsValue> {
        let mut opt = cell.borrow_mut();
        let state = opt.get_or_insert_with(SpectrumState::new);
        let canvas_width = canvas.width();
        let canvas_height = canvas.height();
        let resized =
            state.waterfall_width != canvas_width || state.waterfall_height != canvas_height;
        state.waterfall_width = canvas_width;
        state.waterfall_height = canvas_height;

        if state.history.is_empty() {
            state.waterfall_initialized = false;
            ctx.set_fill_style_str(bg);
            ctx.fill_rect(0.0, 0.0, width, height);
            ctx.set_stroke_style_str(axis);
            ctx.stroke_rect(0.5, 0.5, (width - 1.0).max(0.0), (height - 1.0).max(0.0));
            ctx.set_fill_style_str(text);
            ctx.set_font("12px sans-serif");
            ctx.fill_text("Waiting for waterfall data...", 12.0, 20.0)?;
            return Ok(());
        }

        let plot_left = AXIS_MARGIN_LEFT.min((width - 1.0).max(0.0));
        let plot_top = AXIS_MARGIN_TOP.min((height - 1.0).max(0.0));
        let plot_width = (width - AXIS_MARGIN_LEFT - AXIS_MARGIN_RIGHT).max(1.0);
        let plot_height = (height - AXIS_MARGIN_TOP - AXIS_MARGIN_BOTTOM).max(1.0);
        let row_height = (plot_height / MAX_WATERFALL_FRAMES as f64).max(1.0);

        if resized || !state.waterfall_initialized {
            ctx.set_fill_style_str(bg);
            ctx.fill_rect(0.0, 0.0, width, height);
            ctx.set_stroke_style_str(axis);
            ctx.stroke_rect(0.5, 0.5, (width - 1.0).max(0.0), (height - 1.0).max(0.0));

            draw_waterfall_axes(
                &ctx,
                grid,
                axis,
                text,
                plot_left,
                plot_top,
                plot_width,
                plot_height,
                state.sample_rate,
            )?;

            let visible_rows = (plot_height / row_height).floor().max(1.0) as usize;
            let first_row = state.history.len().saturating_sub(visible_rows);
            for (row_idx, frame) in state.history.iter().skip(first_row).enumerate() {
                let y = plot_top + plot_height
                    - (state.history.len() - first_row - row_idx) as f64 * row_height;
                draw_waterfall_row(&ctx, frame, plot_left, y, plot_width, row_height);
            }
            state.waterfall_initialized = true;
        } else if let Some(frame) = state.history.back() {
            let copy_height = (plot_height - row_height).max(1.0);
            ctx.draw_image_with_html_canvas_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                &canvas,
                plot_left,
                plot_top + row_height,
                plot_width,
                copy_height,
                plot_left,
                plot_top,
                plot_width,
                copy_height,
            )?;
            let y = plot_top + plot_height - row_height;
            ctx.set_fill_style_str(bg);
            ctx.fill_rect(plot_left, y, plot_width, row_height);
            draw_waterfall_row(&ctx, frame, plot_left, y, plot_width, row_height);
        }

        Ok(())
    })?;

    Ok(())
}

fn data_range(values: &[f32]) -> Option<(f32, f32)> {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &value in values {
        if !value.is_finite() {
            continue;
        }
        min = min.min(value);
        max = max.max(value);
    }
    if min.is_finite() && max.is_finite() {
        Some((min, max))
    } else {
        None
    }
}

fn draw_waterfall_row(
    ctx: &CanvasRenderingContext2d,
    frame: &[f32],
    plot_left: f64,
    y: f64,
    plot_width: f64,
    row_height: f64,
) {
    if frame.is_empty() {
        return;
    }
    let len = frame.len();
    let half = len / 2;
    let bin_width = (plot_width / len.max(1) as f64).max(1.0);
    for i in 0..len {
        let value = frame[(i + half) % len];
        ctx.set_fill_style_str(waterfall_color(value));
        ctx.fill_rect(
            plot_left + i as f64 * plot_width / len as f64,
            y,
            bin_width,
            row_height,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_axes(
    ctx: &CanvasRenderingContext2d,
    grid: &str,
    axis: &str,
    text: &str,
    plot_left: f64,
    plot_top: f64,
    plot_width: f64,
    plot_height: f64,
    sample_rate: f32,
    y_min: f64,
    y_max: f64,
) -> Result<(), JsValue> {
    let plot_right = plot_left + plot_width;
    let plot_bottom = plot_top + plot_height;

    ctx.set_stroke_style_str(grid);
    ctx.set_line_width(1.0);
    for i in 0..AXIS_TICK_COUNT {
        let t = if AXIS_TICK_COUNT <= 1 {
            0.0
        } else {
            i as f64 / (AXIS_TICK_COUNT - 1) as f64
        };
        let x = plot_left + t * plot_width;
        let y = plot_top + t * plot_height;
        ctx.begin_path();
        ctx.move_to(x, plot_top);
        ctx.line_to(x, plot_bottom);
        ctx.stroke();
        ctx.begin_path();
        ctx.move_to(plot_left, y);
        ctx.line_to(plot_right, y);
        ctx.stroke();
    }

    ctx.set_stroke_style_str(axis);
    ctx.begin_path();
    ctx.move_to(plot_left, plot_top);
    ctx.line_to(plot_left, plot_bottom);
    ctx.line_to(plot_right, plot_bottom);
    ctx.stroke();

    ctx.set_fill_style_str(text);
    ctx.set_font("12px sans-serif");
    ctx.set_text_align("center");
    ctx.set_text_baseline("top");
    for i in 0..AXIS_TICK_COUNT {
        let t = if AXIS_TICK_COUNT <= 1 {
            0.0
        } else {
            i as f64 / (AXIS_TICK_COUNT - 1) as f64
        };
        let freq = (t - 0.5) * f64::from(sample_rate);
        ctx.fill_text(
            &format_hz(freq),
            plot_left + t * plot_width,
            plot_bottom + 6.0,
        )?;
    }

    ctx.set_text_align("right");
    ctx.set_text_baseline("middle");
    for i in 0..AXIS_TICK_COUNT {
        let t = if AXIS_TICK_COUNT <= 1 {
            0.0
        } else {
            i as f64 / (AXIS_TICK_COUNT - 1) as f64
        };
        let value = y_max - t * (y_max - y_min);
        ctx.fill_text(
            &format!("{value:.0}"),
            plot_left - 6.0,
            plot_top + t * plot_height,
        )?;
    }

    ctx.set_text_align("center");
    ctx.set_text_baseline("top");
    ctx.fill_text(
        "Frequency (Hz)",
        plot_left + plot_width / 2.0,
        plot_bottom + 20.0,
    )?;

    ctx.save();
    ctx.translate(plot_left - 40.0, plot_top + plot_height / 2.0)?;
    ctx.rotate(-std::f64::consts::FRAC_PI_2)?;
    ctx.set_text_align("center");
    ctx.set_text_baseline("top");
    ctx.fill_text("Power (dB)", 0.0, 0.0)?;
    ctx.restore();

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_waterfall_axes(
    ctx: &CanvasRenderingContext2d,
    grid: &str,
    axis: &str,
    text: &str,
    plot_left: f64,
    plot_top: f64,
    plot_width: f64,
    plot_height: f64,
    sample_rate: f32,
) -> Result<(), JsValue> {
    let plot_right = plot_left + plot_width;
    let plot_bottom = plot_top + plot_height;

    ctx.set_stroke_style_str(grid);
    ctx.set_line_width(1.0);
    for i in 0..AXIS_TICK_COUNT {
        let t = if AXIS_TICK_COUNT <= 1 {
            0.0
        } else {
            i as f64 / (AXIS_TICK_COUNT - 1) as f64
        };
        let x = plot_left + t * plot_width;
        ctx.begin_path();
        ctx.move_to(x, plot_top);
        ctx.line_to(x, plot_bottom);
        ctx.stroke();
    }

    ctx.set_stroke_style_str(axis);
    ctx.begin_path();
    ctx.move_to(plot_left, plot_top);
    ctx.line_to(plot_left, plot_bottom);
    ctx.line_to(plot_right, plot_bottom);
    ctx.stroke();

    ctx.set_fill_style_str(text);
    ctx.set_font("12px sans-serif");
    ctx.set_text_align("center");
    ctx.set_text_baseline("top");
    for i in 0..AXIS_TICK_COUNT {
        let t = if AXIS_TICK_COUNT <= 1 {
            0.0
        } else {
            i as f64 / (AXIS_TICK_COUNT - 1) as f64
        };
        let freq = (t - 0.5) * f64::from(sample_rate);
        ctx.fill_text(
            &format_hz(freq),
            plot_left + t * plot_width,
            plot_bottom + 6.0,
        )?;
    }

    ctx.set_text_align("center");
    ctx.set_text_baseline("top");
    ctx.fill_text(
        "Frequency (Hz)",
        plot_left + plot_width / 2.0,
        plot_bottom + 20.0,
    )?;

    ctx.save();
    ctx.translate(plot_left - 40.0, plot_top + plot_height / 2.0)?;
    ctx.rotate(-std::f64::consts::FRAC_PI_2)?;
    ctx.set_text_align("center");
    ctx.set_text_baseline("top");
    ctx.fill_text("Time", 0.0, 0.0)?;
    ctx.restore();

    Ok(())
}

fn waterfall_color(value: f32) -> &'static str {
    const COLORS: [&str; 16] = [
        "#00165f", "#002a86", "#0040ad", "#0059c8", "#0074d0", "#008fc2", "#00a9aa", "#17bd8b",
        "#4cc869", "#84ce4b", "#bfd13d", "#e8ca39", "#f5ae32", "#f58a2d", "#ee632f", "#ebebeb",
    ];
    if !value.is_finite() {
        return COLORS[0];
    }
    let t = ((value - WATERFALL_MIN_DB) / (WATERFALL_MAX_DB - WATERFALL_MIN_DB)).clamp(0.0, 1.0);
    let idx = (t * (COLORS.len() - 1) as f32).round() as usize;
    COLORS[idx]
}

fn format_hz(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1000.0 {
        format!("{:.1}k", value / 1000.0)
    } else {
        format!("{value:.0}")
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn update(frames: Vec<FloatPduStream>) -> Result<(), JsValue> {
    with_spectrum_state(|state| state.set_latest(frames));
    draw_all()
}
