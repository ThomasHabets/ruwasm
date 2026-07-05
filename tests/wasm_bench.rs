use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn bench_demod_inner_loop() {
    //let input = make_test_input();
    let input = &[0.0f32];
    let _output = vec![0.0f32; input.len()];

    // Warm up browser JIT / Wasm compilation paths.
    for _ in 0..100 {
        //my_hot_function(&input, &mut output);
    }

    let start = web_sys::window().unwrap().performance().unwrap().now();

    let iters = 1_000;
    for _ in 0..iters {
        //my_hot_function(&input, &mut output);
    }

    let end = web_sys::window().unwrap().performance().unwrap().now();

    let ns_per_iter = (end - start) * 1_000_000.0 / f64::from(iters);

    web_sys::console::log_1(&format!("bench_demod_inner_loop: {ns_per_iter:.1} ns/iter").into());

    // Optional hard regression gate.
    assert!(
        ns_per_iter < 50_000.0,
        "benchmark regression: {ns_per_iter:.1} ns/iter"
    );
}
