/// Mostly vibe coded time sink graph drawer.
///
/// It works as far as getting something on the screen, but requires more work.
///
/// Things that need fixing:
/// * Y axis labels.
/// * Toggle for auto scaling, not button.
/// * The whole thing should get the complete new set of points, not append and
///   trim.
use std::cell::RefCell;
use std::collections::VecDeque;

use log::{debug, info};
use wasm_bindgen::prelude::*;
use web_sys::{
    CanvasRenderingContext2d, Event, HtmlButtonElement, HtmlCanvasElement, HtmlInputElement,
};

use crate::FloatStream;
use crate::mainthread::get_element;

const ID_GRAPH_CANVAS: &str = "float-graph";
const ID_GRAPH_Y_MIN: &str = "graph-y-min";
const ID_GRAPH_Y_MAX: &str = "graph-y-max";
const ID_GRAPH_Y_APPLY: &str = "graph-y-apply";
const ID_GRAPH_Y_ZOOM_IN: &str = "graph-y-zoom-in";
const ID_GRAPH_Y_ZOOM_OUT: &str = "graph-y-zoom-out";
const ID_GRAPH_Y_AUTO: &str = "graph-y-auto";

const MAX_GRAPH_POINTS: usize = 500_000;

thread_local! {
    static GRAPH_STATE: RefCell<Option<GraphState>> = const { RefCell::new(None) };
}

/// TODO: this is wrong. The series should be aligned, not treated separately.
struct GraphSeries {
    name: String,
    start_index: u64,
    samples: VecDeque<f32>,
}

impl GraphSeries {
    fn new(name: String) -> Self {
        Self {
            name,
            start_index: 0,
            samples: VecDeque::new(),
        }
    }

    fn append_samples(&mut self, samples: &[f32]) {
        self.samples.extend(samples.iter().copied());
        while self.samples.len() > MAX_GRAPH_POINTS {
            self.samples.pop_front();
            self.start_index = self.start_index.saturating_add(1);
        }
    }
}

struct GraphState {
    series: Vec<GraphSeries>,
    y_min: f32,
    y_max: f32,
    auto_scale: bool,
    sample_rate: f64,
    sync_inputs: bool,
}

impl GraphState {
    fn new() -> Self {
        Self {
            series: Vec::new(),
            y_min: -1.0,
            y_max: 1.0,
            auto_scale: true,
            sample_rate: 1.0,
            sync_inputs: true,
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        if sample_rate.is_finite() && sample_rate > 0.0 {
            self.sample_rate = sample_rate;
        }
    }

    fn append_streams(&mut self, streams: &[FloatStream]) {
        for stream in streams {
            let idx = self.series.iter().position(|s| s.name == stream.name);
            let series = match idx {
                Some(i) => &mut self.series[i],
                None => {
                    self.series.push(GraphSeries::new(stream.name.clone()));
                    self.series.last_mut().expect("series just added")
                }
            };
            series.append_samples(&stream.samples);
        }
    }
}

fn with_graph_state<T>(f: impl FnOnce(&mut GraphState) -> T) -> T {
    GRAPH_STATE.with(|cell| {
        let mut opt = cell.borrow_mut();
        let state = opt.get_or_insert_with(GraphState::new);
        f(state)
    })
}

pub(crate) fn setup_graph_ui() -> Result<(), JsValue> {
    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            let y_min = parse_f32_input(ID_GRAPH_Y_MIN)?;
            let y_max = parse_f32_input(ID_GRAPH_Y_MAX)?;
            if !y_min.is_finite() || !y_max.is_finite() || y_min >= y_max {
                return Err(JsValue::from_str("invalid Y min/max range"));
            }
            with_graph_state(|state| {
                state.y_min = y_min;
                state.y_max = y_max;
                state.auto_scale = false;
                state.sync_inputs = false;
            });
            draw_graph()?;
            Ok(())
        });
        let btn = get_element(ID_GRAPH_Y_APPLY)?.dyn_into::<HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            zoom_y(0.8)?;
            Ok(())
        });
        let btn = get_element(ID_GRAPH_Y_ZOOM_IN)?.dyn_into::<HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            zoom_y(1.25)?;
            Ok(())
        });
        let btn = get_element(ID_GRAPH_Y_ZOOM_OUT)?.dyn_into::<HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let handler = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
            with_graph_state(|state| {
                state.auto_scale = true;
                state.sync_inputs = true;
            });
            draw_graph()?;
            Ok(())
        });
        let btn = get_element(ID_GRAPH_Y_AUTO)?.dyn_into::<HtmlButtonElement>()?;
        btn.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let handler = Closure::<dyn FnMut(Event)>::new(move |_e: Event| {
            let _ = draw_graph();
        });
        let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
        window.add_event_listener_with_callback("resize", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    with_graph_state(|state| state.sync_inputs = true);
    draw_graph()?;
    Ok(())
}

fn data_range(state: &GraphState) -> Option<(f32, f32)> {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for series in &state.series {
        for &sample in &series.samples {
            if sample < min {
                min = sample;
            }
            if sample > max {
                max = sample;
            }
        }
    }
    if min.is_finite() && max.is_finite() {
        Some((min, max))
    } else {
        None
    }
}

fn time_range(state: &GraphState) -> Option<(f64, f64)> {
    let mut min_idx: Option<u64> = None;
    let mut max_idx: Option<u64> = None;
    for series in &state.series {
        let len = series.samples.len() as u64;
        if len == 0 {
            continue;
        }
        let series_min = series.start_index;
        let series_max = series.start_index + len - 1;
        min_idx = Some(min_idx.map_or(series_min, |v| v.min(series_min)));
        max_idx = Some(max_idx.map_or(series_max, |v| v.max(series_max)));
    }
    let sample_rate = if state.sample_rate > 0.0 {
        state.sample_rate
    } else {
        1.0
    };
    match (min_idx, max_idx) {
        (Some(min_idx), Some(max_idx)) => {
            let min_t = min_idx as f64 / sample_rate;
            let max_t = max_idx as f64 / sample_rate;
            Some((min_t, max_t))
        }
        _ => None,
    }
}

fn resize_canvas_to_display_size(canvas: &HtmlCanvasElement) -> Result<(f64, f64), JsValue> {
    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
    let dpr = window.device_pixel_ratio();
    let display_width = canvas.client_width() as f64;
    let display_height = canvas.client_height() as f64;
    if display_width > 0.0 && display_height > 0.0 {
        let width = (display_width * dpr).round().max(1.0) as u32;
        let height = (display_height * dpr).round().max(1.0) as u32;
        if canvas.width() != width || canvas.height() != height {
            canvas.set_width(width);
            canvas.set_height(height);
        }
    }
    Ok((canvas.width() as f64, canvas.height() as f64))
}

fn draw_graph() -> Result<(), JsValue> {
    let canvas = get_element(ID_GRAPH_CANVAS)?.dyn_into::<HtmlCanvasElement>()?;
    let (width, height) = resize_canvas_to_display_size(&canvas)?;
    let ctx = canvas
        .get_context("2d")?
        .ok_or(JsValue::from_str("no 2d context"))?
        .dyn_into::<CanvasRenderingContext2d>()?;

    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;
    let is_dark = window
        .match_media("(prefers-color-scheme: dark)")?
        .map(|m| m.matches())
        .unwrap_or(false);
    let bg = if is_dark { "#0b0b0b" } else { "#ffffff" };
    let axis = if is_dark { "#666" } else { "#888" };
    let text = if is_dark { "#ddd" } else { "#222" };

    ctx.set_fill_style(&JsValue::from_str(bg));
    ctx.fill_rect(0.0, 0.0, width, height);
    ctx.set_stroke_style(&JsValue::from_str(axis));
    ctx.stroke_rect(0.5, 0.5, (width - 1.0).max(0.0), (height - 1.0).max(0.0));

    GRAPH_STATE.with(|cell| -> Result<(), JsValue> {
        let mut opt = cell.borrow_mut();
        let state = opt.get_or_insert_with(GraphState::new);
        if state.auto_scale {
            if let Some((min, max)) = data_range(state) {
                info!("Data range: {min} {max}");
                state.y_min = min;
                state.y_max = max;
            }
            state.sync_inputs = true;
        }

        if state.sync_inputs {
            set_y_inputs(state.y_min, state.y_max)?;
            state.sync_inputs = false;
        }

        let mut y_min = state.y_min;
        let mut y_max = state.y_max;
        if !y_min.is_finite() || !y_max.is_finite() || y_min >= y_max {
            y_min = -1.0;
            y_max = 1.0;
        }
        if (y_max - y_min).abs() < f32::EPSILON {
            y_min -= 1.0;
            y_max += 1.0;
        }

        let Some((x_min, x_max)) = time_range(state) else {
            ctx.set_fill_style(&JsValue::from_str(text));
            ctx.set_font("12px sans-serif");
            ctx.fill_text("Waiting for float data...", 12.0, 20.0)?;
            return Ok(());
        };

        let x_range = (x_max - x_min).max(1e-9);
        let y_range = (y_max - y_min) as f64;
        let colors = ["#2b8cbe", "#31a354", "#756bb1", "#e6550d"];

        for (idx, series) in state.series.iter().enumerate() {
            if series.samples.is_empty() {
                continue;
            }
            let len = series.samples.len();
            let mut step = (len as f64 / width.max(1.0)).ceil() as usize;
            if step < 1 {
                step = 1;
            }
            ctx.set_stroke_style(&JsValue::from_str(colors[idx % colors.len()]));
            ctx.set_line_width(1.0);
            ctx.begin_path();
            let mut started = false;
            for (i, sample) in series.samples.iter().enumerate().step_by(step) {
                let sample_idx = series.start_index + i as u64;
                let t = sample_idx as f64 / state.sample_rate;
                let x = ((t - x_min) / x_range) * width;
                let y = height - ((*sample as f64 - y_min as f64) / y_range) * height;
                if !started {
                    ctx.move_to(x, y);
                    started = true;
                } else {
                    ctx.line_to(x, y);
                }
            }
            ctx.stroke();
        }
        Ok(())
    })?;

    Ok(())
}

fn zoom_y(factor: f32) -> Result<(), JsValue> {
    with_graph_state(|state| {
        let (mut y_min, mut y_max) = if state.auto_scale {
            data_range(state).unwrap_or((state.y_min, state.y_max))
        } else {
            (state.y_min, state.y_max)
        };
        if !y_min.is_finite() || !y_max.is_finite() || y_min >= y_max {
            y_min = -1.0;
            y_max = 1.0;
        }
        let center = (y_min + y_max) / 2.0;
        let half = ((y_max - y_min) / 2.0 * factor).max(1e-6);
        state.y_min = center - half;
        state.y_max = center + half;
        state.auto_scale = false;
        state.sync_inputs = true;
    });
    draw_graph()
}
fn set_y_inputs(y_min: f32, y_max: f32) -> Result<(), JsValue> {
    get_element(ID_GRAPH_Y_MIN)?
        .dyn_into::<HtmlInputElement>()?
        .set_value(&format!("{y_min:}"));
    get_element(ID_GRAPH_Y_MAX)?
        .dyn_into::<HtmlInputElement>()?
        .set_value(&format!("{y_max:}"));
    Ok(())
}

fn parse_f32_input(id: &str) -> Result<f32, JsValue> {
    get_element(id)?
        .dyn_into::<HtmlInputElement>()?
        .value()
        .parse::<f32>()
        .map_err(|e| JsValue::from_str(&format!("parsing {id}: {e}")))
}

pub(crate) fn update(streams: Vec<FloatStream>) -> Result<(), JsValue> {
    with_graph_state(|state| state.append_streams(&streams));
    draw_graph()
}

pub(crate) fn set_sample_rate(samp_rate: f64) {
    with_graph_state(|state| state.set_sample_rate(samp_rate));
}
