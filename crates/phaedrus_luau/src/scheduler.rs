//! Cooperative thread scheduler backing `task.*`, `wait` and `:Wait()`.

use std::collections::VecDeque;
use std::time::Instant;

use mlua::thread::ThreadStatus;
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Result, Thread, Value};

use crate::signal::Signal;
use crate::{OutputLevel, ctx};

struct Sleeper {
    wake: f64,
    started: f64,
    thread: Thread,
}

#[derive(Default)]
pub struct Scheduler {
    sleeping: Vec<Sleeper>,
    deferred: VecDeque<(Thread, MultiValue)>,
}

impl Scheduler {
    pub fn is_idle(&self) -> bool {
        self.sleeping.is_empty() && self.deferred.is_empty()
    }

    pub fn cancel(&mut self, thread: &Thread) {
        self.sleeping.retain(|s| &s.thread != thread);
        self.deferred.retain(|(t, _)| t != thread);
    }

    pub fn defer(&mut self, thread: Thread, args: MultiValue) {
        self.deferred.push_back((thread, args));
    }
}

/// Creates a thread for `f` and runs it immediately until it yields.
pub fn spawn_function(lua: &Lua, f: Function, args: impl IntoLuaMulti) -> Result<Thread> {
    let thread = lua.create_thread(f)?;
    let args = args.into_lua_multi(lua)?;
    resume(lua, thread.clone(), args);
    Ok(thread)
}

/// Resumes a thread and handles whatever it yields. Errors are reported to
/// the output rather than propagated (like Roblox).
pub fn resume(lua: &Lua, thread: Thread, args: MultiValue) {
    if thread.status() != ThreadStatus::Resumable {
        return;
    }
    let c = ctx(lua);
    let outer = c.resume_started.get();
    if outer.is_none() {
        c.resume_started.set(Some(Instant::now()));
    }
    drop(c);
    let result = thread.resume::<MultiValue>(args);
    let c = ctx(lua);
    if outer.is_none() {
        c.resume_started.set(None);
    }
    match result {
        Ok(values) => {
            if thread.status() == ThreadStatus::Resumable {
                drop(c);
                handle_yield(lua, thread, values);
            }
        }
        Err(e) => {
            c.print(OutputLevel::Error, format_error(&e));
        }
    }
}

fn handle_yield(lua: &Lua, thread: Thread, values: MultiValue) {
    let c = ctx(lua);
    let mut it = values.into_iter();
    let first = it.next();
    match first {
        Some(Value::Table(t)) if t == c.wait_sentinel => {
            let secs = match it.next() {
                Some(Value::Integer(i)) => i as f64,
                Some(Value::Number(n)) => n,
                _ => 0.0,
            };
            let now = c.clock();
            // A zero wait resumes on the next step, like Roblox's minimum
            // of one frame.
            c.sched.borrow_mut().sleeping.push(Sleeper { wake: now + secs.max(0.0), started: now, thread });
        }
        Some(Value::Table(t)) if t == c.signal_sentinel => {
            if let Some(Value::UserData(ud)) = it.next()
                && let Ok(sig) = ud.borrow::<Signal>() {
                    c.signals.borrow_mut().add_waiter(sig.key.clone(), thread);
                    return;
                }
            c.print(OutputLevel::Error, "invalid signal passed to Wait".into());
        }
        _ => {
            // Plain coroutine.yield() from a script thread: resume next step.
            c.sched.borrow_mut().defer(thread, MultiValue::new());
        }
    }
}

/// Runs all threads whose wait has elapsed, then deferred threads.
pub fn step(lua: &Lua) {
    let c = ctx(lua);
    let now = c.clock();
    let ready: Vec<Sleeper> = {
        let mut s = c.sched.borrow_mut();
        let (ready, waiting): (Vec<_>, Vec<_>) = s.sleeping.drain(..).partition(|x| x.wake <= now);
        s.sleeping = waiting;
        ready
    };
    let deferred: Vec<(Thread, MultiValue)> = c.sched.borrow_mut().deferred.drain(..).collect();
    drop(c);
    for s in ready {
        let elapsed = now - s.started;
        let args = (elapsed, now).into_lua_multi(lua).unwrap_or_default();
        resume(lua, s.thread, args);
    }
    for (t, args) in deferred {
        resume(lua, t, args);
    }
}

pub fn format_error(e: &mlua::Error) -> String {
    match e {
        mlua::Error::CallbackError { traceback, cause } => {
            format!("{}\n{}", format_error(cause), traceback.trim())
        }
        mlua::Error::RuntimeError(s) => s.clone(),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::WithContext { context, cause } => format!("{context}: {}", format_error(cause)),
        other => other.to_string(),
    }
}
