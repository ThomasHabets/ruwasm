use serde::Serialize;

#[derive(Serialize)]
struct Return {
    a: i32,
    b: i32,
    sum: i32,
    eval: String,
}

#[unsafe(no_mangle)]
pub extern "C" fn free_string(ptr: *mut u8, len: usize) {
    unsafe {
        let _ = Box::from_raw(core::slice::from_raw_parts_mut(ptr, len));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn compute(n: u32) -> u32 {
    (0..n).map(|x| x * x).sum()
}

#[unsafe(no_mangle)]
pub extern "C" fn add(a: i32, b: i32) -> u64 {
    let s = serde_json::to_string(&Return {
        a,
        b,
        sum: a + b,
        eval: "console.log('hello world')".to_string(),
    })
    .unwrap();
    let boxed: Box<str> = s.into_boxed_str();

    let ptr = boxed.as_ptr() as u32;
    let len = boxed.len() as u32;

    // Leak the box so Rust doesn't deallocate it
    core::mem::forget(boxed);
    ((ptr as u64) << 32) | len as u64
}

#[cfg(test)]
mod tests {
    use super::*;
}
