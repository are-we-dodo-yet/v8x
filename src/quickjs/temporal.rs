use super::quickjs_sys::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::{OnceLock, RwLock};
use temporal_rs::{Instant, TimeZone};
use timezone_provider::zoneinfo64::ZoneInfo64TzdbProvider;

fn provider() -> &'static ZoneInfo64TzdbProvider<'static> {
  static PROVIDER: OnceLock<ZoneInfo64TzdbProvider<'static>> = OnceLock::new();
  PROVIDER.get_or_init(|| {
    ZoneInfo64TzdbProvider::zoneinfo64_provider_for_testing()
      .expect("bundled zoneinfo64 data is invalid")
  })
}

const UNKNOWN_TIME_ZONE: &str = "Etc/Unknown";

static DEFAULT_TIME_ZONE: RwLock<Option<(String, TimeZone)>> =
  RwLock::new(None);

fn parse_time_zone(id: &str) -> Option<TimeZone> {
  if id == UNKNOWN_TIME_ZONE {
    return None;
  }
  TimeZone::try_from_identifier_str_with_provider(id, provider()).ok()
}

fn zoneinfo_suffix(path: &str) -> Option<&str> {
  path.rsplit_once("zoneinfo/").map(|(_, id)| id)
}

fn host_time_zone() -> String {
  #[cfg(unix)]
  {
    if let Ok(tz) = std::env::var("TZ") {
      let tz = tz.strip_prefix(':').unwrap_or(&tz);
      let id = zoneinfo_suffix(tz).unwrap_or(tz);
      if parse_time_zone(id).is_some() {
        return id.to_string();
      }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime")
      && let Some(id) = target.to_str().and_then(zoneinfo_suffix)
      && parse_time_zone(id).is_some()
    {
      return id.to_string();
    }
  }
  UNKNOWN_TIME_ZONE.to_string()
}

fn default_time_zone() -> Option<TimeZone> {
  DEFAULT_TIME_ZONE
    .read()
    .ok()
    .and_then(|guard| guard.as_ref().map(|(_, time_zone)| *time_zone))
}

#[unsafe(no_mangle)]
pub extern "C" fn icu_get_default_time_zone(
  output: *mut c_char,
  output_len: usize,
) -> usize {
  let id = DEFAULT_TIME_ZONE
    .read()
    .ok()
    .and_then(|guard| guard.as_ref().map(|(id, _)| id.clone()))
    .unwrap_or_else(host_time_zone);
  if output.is_null() {
    return 0;
  }
  let len = id.len().min(output_len);
  unsafe {
    std::ptr::copy_nonoverlapping(id.as_ptr(), output.cast::<u8>(), len)
  };
  len
}

#[unsafe(no_mangle)]
pub extern "C" fn icu_set_default_time_zone(
  time_zone_id: *const c_char,
) -> bool {
  if time_zone_id.is_null() {
    return false;
  }
  let Ok(id) = unsafe { CStr::from_ptr(time_zone_id) }.to_str() else {
    return false;
  };
  let Some(time_zone) = parse_time_zone(id) else {
    return false;
  };
  let value = (id != host_time_zone()).then(|| (id.to_string(), time_zone));
  let Ok(mut guard) = DEFAULT_TIME_ZONE.write() else {
    return false;
  };
  *guard = value;
  true
}

#[unsafe(no_mangle)]
pub extern "C" fn v82jsc_time_zone_offset(
  time: i64,
  offset: *mut c_int,
) -> c_int {
  let Some(time_zone) = default_time_zone() else {
    return 0;
  };
  let Ok(instant) = Instant::from_epoch_milliseconds(time) else {
    return 0;
  };
  let Ok(zoned) =
    instant.to_zoned_date_time_iso_with_provider(time_zone, provider())
  else {
    return 0;
  };
  if !offset.is_null() {
    unsafe {
      *offset = (-(zoned.offset_nanoseconds() / 60_000_000_000)) as c_int
    };
  }
  1
}

#[unsafe(no_mangle)]
pub extern "C" fn v82jsc_time_zone_name(
  _time: i64,
  _buf: *mut c_char,
  _buf_size: usize,
  len: *mut usize,
) -> c_int {
  if default_time_zone().is_none() {
    return 0;
  }
  if !len.is_null() {
    unsafe { *len = 0 };
  }
  1
}

unsafe fn value_to_string(
  ctx: *mut JSContext,
  value: JSValue,
) -> Option<String> {
  let mut len = 0usize;
  let ptr = unsafe { JS_ToCStringLen(ctx, &mut len, value) };
  if ptr.is_null() {
    return None;
  }
  let text = unsafe {
    String::from_utf8_lossy(std::slice::from_raw_parts(ptr as *const u8, len))
      .into_owned()
  };
  unsafe { JS_FreeCString(ctx, ptr) };
  Some(text)
}

unsafe fn set_i32(
  ctx: *mut JSContext,
  object: JSValue,
  name: &std::ffi::CStr,
  value: i32,
) {
  unsafe {
    JS_SetPropertyStr(ctx, object, name.as_ptr(), JS_NewInt32(ctx, value));
  }
}

unsafe fn set_string(
  ctx: *mut JSContext,
  object: JSValue,
  name: &std::ffi::CStr,
  value: &str,
) {
  unsafe {
    JS_SetPropertyStr(
      ctx,
      object,
      name.as_ptr(),
      JS_NewStringLen(ctx, value.as_ptr() as *const c_char, value.len()),
    );
  }
}

unsafe fn throw_range_error(ctx: *mut JSContext, message: &str) -> JSValue {
  let message = CString::new(message).unwrap_or_default();
  unsafe { JS_ThrowRangeError(ctx, c"%s".as_ptr(), message.as_ptr()) }
}

unsafe extern "C" fn format_time_zone(
  ctx: *mut JSContext,
  _this: JSValue,
  argc: c_int,
  argv: *mut JSValue,
) -> JSValue {
  if argc < 2 || argv.is_null() {
    return unsafe {
      JS_ThrowTypeError(
        ctx,
        c"epoch milliseconds and time zone are required".as_ptr(),
      )
    };
  }

  let mut milliseconds = 0.0;
  if unsafe { JS_ToFloat64(ctx, &mut milliseconds, *argv) } < 0 {
    return jsv_exception();
  }
  if !milliseconds.is_finite()
    || milliseconds < i64::MIN as f64
    || milliseconds > i64::MAX as f64
  {
    return unsafe { throw_range_error(ctx, "invalid epoch milliseconds") };
  }
  let Some(identifier) = (unsafe { value_to_string(ctx, *argv.add(1)) }) else {
    return jsv_exception();
  };

  let provider = provider();
  let Ok(time_zone) =
    TimeZone::try_from_identifier_str_with_provider(&identifier, provider)
  else {
    return unsafe {
      throw_range_error(ctx, &format!("invalid time zone: {identifier}"))
    };
  };
  let Ok(primary_time_zone) =
    time_zone.primary_identifier_with_provider(provider)
  else {
    return unsafe {
      throw_range_error(ctx, &format!("invalid time zone: {identifier}"))
    };
  };
  let Ok(canonical_identifier) =
    primary_time_zone.identifier_with_provider(provider)
  else {
    return unsafe {
      throw_range_error(ctx, &format!("invalid time zone: {identifier}"))
    };
  };
  let Ok(instant) =
    Instant::from_epoch_milliseconds(milliseconds.trunc() as i64)
  else {
    return unsafe {
      throw_range_error(ctx, "epoch milliseconds out of range")
    };
  };
  let Ok(zoned) =
    instant.to_zoned_date_time_iso_with_provider(time_zone, provider)
  else {
    return unsafe {
      throw_range_error(ctx, &format!("invalid time zone: {identifier}"))
    };
  };

  let object = unsafe { JS_NewObject(ctx) };
  unsafe {
    set_string(ctx, object, c"timeZone", &canonical_identifier);
    set_string(
      ctx,
      object,
      c"era",
      if zoned.year() <= 0 { "BC" } else { "AD" },
    );
    set_i32(ctx, object, c"year", zoned.year());
    set_i32(ctx, object, c"month", i32::from(zoned.month()));
    set_i32(ctx, object, c"day", i32::from(zoned.day()));
    set_i32(ctx, object, c"hour", i32::from(zoned.hour()));
    set_i32(ctx, object, c"minute", i32::from(zoned.minute()));
    set_i32(ctx, object, c"second", i32::from(zoned.second()));
    JS_SetPropertyStr(
      ctx,
      object,
      c"offsetNanoseconds".as_ptr(),
      JS_NewInt64(ctx, zoned.offset_nanoseconds()),
    );
  }
  object
}

pub(crate) unsafe fn install_host_functions(
  ctx: *mut JSContext,
  global: JSValue,
) {
  let function = unsafe {
    JS_NewCFunction(ctx, format_time_zone, c"__v8xTemporalTimeZone".as_ptr(), 2)
  };
  unsafe {
    super::core::define_internal_global(
      ctx,
      global,
      c"__v8xTemporalTimeZone",
      function,
    );
  }
}

const JS_WRITE_OBJ_BYTECODE: c_int = 1 << 0;
const JS_READ_OBJ_BYTECODE: c_int = 1 << 0;

unsafe fn eval_cached_script(
  ctx: *mut JSContext,
  source: &[u8],
  filename: &std::ffi::CStr,
  cache: &OnceLock<Vec<u8>>,
) -> JSValue {
  if let Some(bytes) = cache.get() {
    let function = unsafe {
      JS_ReadObject(ctx, bytes.as_ptr(), bytes.len(), JS_READ_OBJ_BYTECODE)
    };
    if function.tag == JS_TAG_EXCEPTION {
      return function;
    }
    return unsafe { JS_EvalFunction(ctx, function) };
  }

  let source = CString::new(source).expect("bootstrap script contains NUL");
  let function = unsafe {
    JS_Eval(
      ctx,
      source.as_ptr(),
      source.as_bytes().len(),
      filename.as_ptr(),
      JS_EVAL_TYPE_GLOBAL | JS_EVAL_FLAG_COMPILE_ONLY,
    )
  };
  if function.tag == JS_TAG_EXCEPTION {
    return function;
  }

  let mut size = 0;
  let bytes =
    unsafe { JS_WriteObject(ctx, &mut size, function, JS_WRITE_OBJ_BYTECODE) };
  if !bytes.is_null() {
    if size > 0 {
      let bytecode = unsafe { std::slice::from_raw_parts(bytes, size) };
      let _ = cache.set(bytecode.to_vec());
    }
    unsafe { js_free(ctx, bytes.cast()) };
  }

  unsafe { JS_EvalFunction(ctx, function) }
}

pub(crate) unsafe fn install(ctx: *mut JSContext, global: JSValue) {
  let existing =
    unsafe { JS_GetPropertyStr(ctx, global, c"Temporal".as_ptr()) };
  let absent = jsv_is_undefined(&existing) || existing.tag == JS_TAG_NULL;
  unsafe { JS_FreeValue(ctx, existing) };
  if !absent {
    return;
  }

  const POLYFILL: &[u8] =
    include_bytes!("../../vendor/temporal-polyfill/index.umd.js");
  static POLYFILL_BYTECODE: OnceLock<Vec<u8>> = OnceLock::new();
  let result = unsafe {
    eval_cached_script(
      ctx,
      POLYFILL,
      c"<temporal-polyfill>",
      &POLYFILL_BYTECODE,
    )
  };
  if result.tag == JS_TAG_EXCEPTION {
    let exception = unsafe { JS_GetException(ctx) };
    let stack = unsafe { JS_GetPropertyStr(ctx, exception, c"stack".as_ptr()) };
    let message = unsafe { value_to_string(ctx, stack) }
      .or_else(|| unsafe { value_to_string(ctx, exception) })
      .unwrap_or_else(|| "unknown exception".to_string());
    unsafe { JS_FreeValue(ctx, stack) };
    eprintln!("failed to initialize Temporal polyfill: {message}");
    unsafe { JS_FreeValue(ctx, exception) };
    return;
  }
  unsafe { JS_FreeValue(ctx, result) };

  const INSTALL: &[u8] = br#"
    (function(g) {
      const implementation = g.temporal;
      if (!implementation || !implementation.Temporal) return;
      const temporal = {};
      for (const name of Object.getOwnPropertyNames(implementation.Temporal)) {
        Object.defineProperty(temporal, name, {
          value: implementation.Temporal[name],
          writable: true,
          configurable: true
        });
      }
      Object.defineProperty(temporal, Symbol.toStringTag, {
        value: "Temporal",
        configurable: true
      });
      Object.defineProperty(g, "Temporal", {
        value: temporal,
        writable: true,
        configurable: true
      });
      if (implementation.Intl && implementation.Intl.DateTimeFormat) {
        g.Intl.DateTimeFormat = implementation.Intl.DateTimeFormat;
      }
      if (implementation.toTemporalInstant) {
        Object.defineProperty(Date.prototype, "toTemporalInstant", {
          value: implementation.toTemporalInstant,
          writable: true,
          configurable: true
        });
      }
      delete g.temporal;
    })(globalThis);
  "#;
  static INSTALL_BYTECODE: OnceLock<Vec<u8>> = OnceLock::new();
  let result = unsafe {
    eval_cached_script(ctx, INSTALL, c"<temporal-install>", &INSTALL_BYTECODE)
  };
  if result.tag == JS_TAG_EXCEPTION {
    let exception = unsafe { JS_GetException(ctx) };
    let message = unsafe { value_to_string(ctx, exception) }
      .unwrap_or_else(|| "unknown exception".to_string());
    eprintln!("failed to install Temporal polyfill: {message}");
    unsafe { JS_FreeValue(ctx, exception) };
  } else {
    unsafe { JS_FreeValue(ctx, result) };
  }
}
