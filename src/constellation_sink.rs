//! Like time sink, this is mostly LLM coded. It does work as a proof of
//! concept, but it needs to be properly reviewed.
use std::cell::RefCell;
use std::collections::VecDeque;

use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, Event, HtmlCanvasElement};

use crate::mainthread::get_element;
use rustradio_ui::ComplexStream;

const ID_CONSTELLATION_CANVAS: &str = "constellation-graph";
const MAX_CONSTELLATION_POINTS: usize = 5_000;
const AXIS_MARGIN: f64 = 28.0;
const GRID_LINES: i32 = 4;

thread_local! {
    static CONSTELLATION_STATE: RefCell<Option<ConstellationState>> = const { RefCell::new(None) };
}

struct ConstellationSeries {
    name: String,
    points: VecDeque<rustradio::Complex>,
}

impl ConstellationSeries {
    fn new(name: String) -> Self {
        Self {
            name,
            points: VecDeque::new(),
        }
    }

    fn append_points(&mut self, points: &[rustradio::Complex]) {
        self.points.extend(points.iter().copied());
        while self.points.len() > MAX_CONSTELLATION_POINTS {
            self.points.pop_front();
        }
    }
}

struct ConstellationState {
    series: Vec<ConstellationSeries>,
}

impl ConstellationState {
    fn new() -> Self {
        Self { series: Vec::new() }
    }

    fn append_streams(&mut self, streams: &[ComplexStream]) {
        for stream in streams {
            let idx = self.series.iter().position(|s| s.name == stream.name);
            let series = if let Some(i) = idx {
                &mut self.series[i]
            } else {
                self.series
                    .push(ConstellationSeries::new(stream.name.clone()));
                self.series.last_mut().expect("series just added")
            };
            series.append_points(&stream.samples);
        }
    }
}

fn with_constellation_state<T>(f: impl FnOnce(&mut ConstellationState) -> T) -> T {
    CONSTELLATION_STATE.with(|cell| {
        let mut opt = cell.borrow_mut();
        let state = opt.get_or_insert_with(ConstellationState::new);
        f(state)
    })
}

pub(crate) fn setup_graph_ui() -> Result<(), JsValue> {
    let handler = Closure::<dyn FnMut(Event)>::new(move |_e: Event| {
        let _ = draw_graph();
    });
    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
    window.add_event_listener_with_callback("resize", handler.as_ref().unchecked_ref())?;
    handler.forget();

    draw_graph()
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
    let canvas = get_element(ID_CONSTELLATION_CANVAS)?.dyn_into::<HtmlCanvasElement>()?;
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
    let colors = ["#2b8cbe", "#31a354", "#756bb1", "#e6550d"];

    ctx.set_fill_style_str(bg);
    ctx.fill_rect(0.0, 0.0, width, height);
    ctx.set_stroke_style_str(axis);
    ctx.stroke_rect(0.5, 0.5, (width - 1.0).max(0.0), (height - 1.0).max(0.0));

    CONSTELLATION_STATE.with(|cell| -> Result<(), JsValue> {
        let mut opt = cell.borrow_mut();
        let state = opt.get_or_insert_with(ConstellationState::new);
        if !state.series.iter().any(|series| !series.points.is_empty()) {
            ctx.set_fill_style_str(text);
            ctx.set_font("12px sans-serif");
            ctx.fill_text("Waiting for IQ data...", 12.0, 20.0)?;
            return Ok(());
        }

        let plot_left = AXIS_MARGIN.min((width - 1.0).max(0.0));
        let plot_top = AXIS_MARGIN.min((height - 1.0).max(0.0));
        let plot_width = (width - AXIS_MARGIN * 2.0).max(1.0);
        let plot_height = (height - AXIS_MARGIN * 2.0).max(1.0);
        let center_x = plot_left + plot_width / 2.0;
        let center_y = plot_top + plot_height / 2.0;
        let plot_radius = plot_width.min(plot_height) / 2.0;
        let max_abs = data_max_abs(state).unwrap_or(1.0).max(1e-6) * 1.08;

        draw_axes(
            &ctx,
            grid,
            axis,
            text,
            center_x,
            center_y,
            plot_radius,
            max_abs,
        )?;

        for (idx, series) in state.series.iter().enumerate() {
            ctx.set_fill_style_str(colors[idx % colors.len()]);
            for point in &series.points {
                if !point.re.is_finite() || !point.im.is_finite() {
                    continue;
                }
                let x = center_x + (f64::from(point.re) / f64::from(max_abs)) * plot_radius;
                let y = center_y - (f64::from(point.im) / f64::from(max_abs)) * plot_radius;
                ctx.fill_rect(x - 1.5, y - 1.5, 3.0, 3.0);
            }
        }
        Ok(())
    })?;

    Ok(())
}

fn data_max_abs(state: &ConstellationState) -> Option<f32> {
    let mut max_abs = 0.0_f32;
    for series in &state.series {
        for point in &series.points {
            if point.re.is_finite() {
                max_abs = max_abs.max(point.re.abs());
            }
            if point.im.is_finite() {
                max_abs = max_abs.max(point.im.abs());
            }
        }
    }
    if max_abs > 0.0 { Some(max_abs) } else { None }
}

#[allow(clippy::too_many_arguments)]
fn draw_axes(
    ctx: &CanvasRenderingContext2d,
    grid: &str,
    axis: &str,
    text: &str,
    center_x: f64,
    center_y: f64,
    plot_radius: f64,
    max_abs: f32,
) -> Result<(), JsValue> {
    let grid_left = center_x - plot_radius;
    let grid_right = center_x + plot_radius;
    let grid_top = center_y - plot_radius;
    let grid_bottom = center_y + plot_radius;

    ctx.set_stroke_style_str(grid);
    ctx.set_line_width(1.0);
    for i in -GRID_LINES..=GRID_LINES {
        let t = f64::from(i) / f64::from(GRID_LINES);
        let x = center_x + t * plot_radius;
        let y = center_y + t * plot_radius;
        ctx.begin_path();
        ctx.move_to(x, grid_top);
        ctx.line_to(x, grid_bottom);
        ctx.stroke();
        ctx.begin_path();
        ctx.move_to(grid_left, y);
        ctx.line_to(grid_right, y);
        ctx.stroke();
    }

    ctx.set_stroke_style_str(axis);
    ctx.begin_path();
    ctx.move_to(grid_left, center_y);
    ctx.line_to(grid_right, center_y);
    ctx.move_to(center_x, grid_top);
    ctx.line_to(center_x, grid_bottom);
    ctx.stroke();

    ctx.set_fill_style_str(text);
    ctx.set_font("12px sans-serif");
    ctx.set_text_align("center");
    ctx.set_text_baseline("top");
    ctx.fill_text("I", grid_right - 6.0, center_y + 6.0)?;
    ctx.set_text_align("left");
    ctx.set_text_baseline("middle");
    ctx.fill_text("Q", center_x + 6.0, grid_top + 8.0)?;
    ctx.set_text_align("right");
    ctx.set_text_baseline("bottom");
    ctx.fill_text(
        &format!("+/-{}", format_tick(f64::from(max_abs))),
        grid_right - 6.0,
        grid_bottom - 6.0,
    )?;

    Ok(())
}

fn format_tick(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1000.0 {
        format!("{value:.0}")
    } else if abs >= 100.0 {
        format!("{value:.1}")
    } else if abs >= 10.0 {
        format!("{value:.2}")
    } else if abs >= 1.0 {
        format!("{value:.3}")
    } else {
        format!("{value:.4}")
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn update(streams: Vec<ComplexStream>) -> Result<(), JsValue> {
    with_constellation_state(|state| state.append_streams(&streams));
    draw_graph()
}
