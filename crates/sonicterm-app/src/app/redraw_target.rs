use parking_lot::Mutex;

/// Clone the current redraw target under a short lock, then invoke the redraw
/// callback only after the guard has been released.
pub(super) fn dispatch<T, F>(redraw_target: &Mutex<Option<T>>, request_redraw: F)
where
    T: Clone,
    F: FnOnce(T),
{
    let redraw_target = redraw_target.lock().clone();
    if let Some(target) = redraw_target {
        request_redraw(target);
    }
}

#[cfg(test)]
#[path = "redraw_target_tests.rs"]
mod redraw_target_tests;
