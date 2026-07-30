//! Safe wrappers around the orct2_host_* functions implemented in
//! src/openrct2/rustbridge/RustBridge.cpp. Every raw extern lives here; the
//! rest of the crate stays unsafe-free.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// Snapshot of one ride crossing the FFI boundary.
///
/// Filled in by `orct2_host_ride_stats` on the C++ side; field meanings:
/// ratings are RCT2 fixed-point (raw 525 = 5.25 excitement), `status` is the
/// game's `RideStatus` enum (0 closed, 1 open, 2 testing, 3 simulating).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RideStats {
    pub id: u16,
    pub ride_type: u16,
    pub status: u8,
    /// Stalls (food/drink/shops/toilets) never receive ratings by design.
    pub is_stall: bool,
    pub has_ratings: bool,
    pub excitement: i16,
    pub intensity: i16,
    pub nausea: i16,
}

/// Track construction cursor: raw world coordinates (32 per tile, z step 8)
/// plus a direction (0-3). Updated in place by `orct2_host_track_place`.
///
/// Bank mapping (TrackRoll enum): 0 = none, 2 = left, 4 = right, 15 = upside_down.
/// Slope mapping (TrackPitch enum): 0 = none, 2 = up25, 4 = up60, 6 = down25,
/// 8 = down60, 10 = up90, 18 = down90.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackCursor {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub direction: u8,
    pub bank: u8,
    pub slope: u8,
}

/// Post-test measurements for scoring. Ratings fixed-point x100; g-forces
/// fixed-point x100; speeds raw game velocity units.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RideDetail {
    pub tested: bool,
    pub crashed: bool,
    pub excitement: i16,
    pub intensity: i16,
    pub nausea: i16,
    pub max_speed: i32,
    pub average_speed: i32,
    /// Raw total length summed over stations, as 16.16 fixed-point metres
    /// (`Ride::getTotalLength()`). Divide by 2^16 for the human-readable metres
    /// the game shows; see `report::ride_length_metres`.
    pub ride_length: i32,
    /// One lap in seconds, as the game measures it during the test and shows
    /// as "Ride time" (the sum of the stations' SegmentTime). 0 until a test
    /// completes.
    pub ride_time: i32,
    pub max_positive_g: i16,
    pub max_negative_g: i16,
    pub max_lateral_g: i16,
    pub total_air_time: u16,
    pub num_drops: u8,
    pub highest_drop: u8,
    pub num_inversions: u8,
}

/// Tile-space bounding box of all track elements in the park, plus the world-z
/// range. Filled by `orct2_host_track_bounds`; found=false when no track exists.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct TrackBounds {
    pub min_tile_x: i32,
    pub min_tile_y: i32,
    pub max_tile_x: i32,
    pub max_tile_y: i32,
    pub min_z: i32,
    pub max_z: i32,
}

/// What the game's own circuit walk finds for a ride, filled by
/// `orct2_host_circuit_stats`.
///
/// `walked` counts the pieces a train actually rides: the walk starts at the
/// station and follows the track the way the vehicle does. `total` counts every
/// piece the ride owns on the map. Track that was placed but left off the
/// ridden loop is the difference, and it is the one thing ratings cannot rule
/// out: they require a complete circuit and a finished test lap, so they say
/// nothing about track hanging off to one side.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct CircuitStats {
    pub walked_pieces: u32,
    pub total_pieces: u32,
    pub orphan_pieces: u32,
    /// The walk returned to its starting piece rather than dead-ending.
    pub looped: bool,
}

#[cfg(not(test))]
unsafe extern "C" {
    fn orct2_host_ride_count() -> u32;
    fn orct2_host_ride_stats(index: u32, out: *mut RideStats) -> bool;
    fn orct2_host_log(msg: *const c_char);
    fn orct2_host_sandbox() -> bool;
    fn orct2_host_surface_z(tile_x: i32, tile_y: i32) -> i32;
    fn orct2_host_ride_create(
        ride_type: u16,
        out_ride_id: *mut u16,
        err: *mut c_char,
        err_len: usize,
    ) -> bool;
    fn orct2_host_track_place(
        ride_id: u16,
        track_type: u16,
        chain_lift: bool,
        speed: u8,
        cursor: *mut TrackCursor,
        out_cost: *mut i64,
        err: *mut c_char,
        err_len: usize,
    ) -> bool;
    fn orct2_host_ride_set_status(
        ride_id: u16,
        status: u8,
        err: *mut c_char,
        err_len: usize,
    ) -> bool;
    fn orct2_host_ride_style(
        ride_id: u16,
        name: *const c_char,
        track_color: *const c_char,
        rail_color: *const c_char,
        support_color: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> bool;
    fn orct2_host_ride_detail(ride_id: u16, out: *mut RideDetail) -> bool;
    fn orct2_host_graphics_available() -> bool;
    fn orct2_host_capture(
        path: *const c_char,
        zoom: i32,
        rotation: u8,
        fit_track: bool,
        xray: bool,
    ) -> bool;
    fn orct2_host_track_bounds(out: *mut TrackBounds) -> bool;
    fn orct2_host_entrance_place(
        ride_id: u16,
        tile_x: i32,
        tile_y: i32,
        direction: u8,
        is_exit: bool,
        err: *mut c_char,
        err_len: usize,
    ) -> bool;
    fn orct2_host_run_ticks(n: u32);
    fn orct2_host_track_query(
        ride_id: u16,
        track_type: u16,
        cursor: *const TrackCursor,
        err: *mut c_char,
        err_len: usize,
    ) -> bool;
    fn orct2_host_ride_demolish(ride_id: u16, err: *mut c_char, err_len: usize) -> bool;
    fn orct2_host_update_ratings(ride_id: u16);
    fn orct2_host_track_remove(
        ride_id: u16,
        track_type: u16,
        origin: *const TrackCursor,
        err: *mut c_char,
        err_len: usize,
    ) -> bool;
    fn orct2_host_piece_delta(track_type: u16, dir_in: u8, out: *mut TrackCursor) -> bool;
    fn orct2_host_track_library_json() -> *mut c_char;
    fn orct2_host_string_free(s: *mut c_char);
    fn orct2_host_track_mirror(track_type: u16) -> u16;
    fn orct2_host_circuit_stats(ride_id: u16, out: *mut CircuitStats) -> bool;
    fn orct2_host_save_park(path: *const c_char) -> bool;
    fn orct2_host_load_park(path: *const c_char) -> bool;
    fn orct2_host_capture_frame(
        zoom: i32,
        rotation: u8,
        fit_track: bool,
        out: *mut u8,
        cap: usize,
    ) -> usize;
    fn orct2_host_capture_size(
        zoom: i32,
        rotation: u8,
        fit_track: bool,
        out_w: *mut u32,
        out_h: *mut u32,
    ) -> bool;
    fn orct2_host_vehicle_status(ride_id: u16, out_status: *mut u8) -> bool;
}

#[cfg(test)]
use test_stubs::*;

const ERR_BUF_LEN: usize = 256;
fn err_buf() -> [c_char; ERR_BUF_LEN] {
    [0; ERR_BUF_LEN]
}

fn err_to_string(buf: &[c_char; ERR_BUF_LEN]) -> String {
    // The C++ side always NUL-terminates within the buffer.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

pub fn log(msg: &str) {
    // Interior NULs would make CString fail; strip them rather than panic.
    let sanitised: String = msg.chars().filter(|&c| c != '\0').collect();
    if let Ok(cstr) = CString::new(sanitised) {
        unsafe { orct2_host_log(cstr.as_ptr()) };
    }
}

pub fn ride_count() -> u32 {
    unsafe { orct2_host_ride_count() }
}

pub fn ride_stats(index: u32) -> Option<RideStats> {
    let mut stats = RideStats {
        id: 0,
        ride_type: 0,
        status: 0,
        is_stall: false,
        has_ratings: false,
        excitement: 0,
        intensity: 0,
        nausea: 0,
    };
    unsafe { orct2_host_ride_stats(index, &mut stats) }.then_some(stats)
}

pub fn enable_sandbox() -> bool {
    unsafe { orct2_host_sandbox() }
}

/// Ground height (raw z units) at a tile coordinate.
pub fn surface_z(tile_x: i32, tile_y: i32) -> i32 {
    unsafe { orct2_host_surface_z(tile_x, tile_y) }
}

pub fn ride_create(ride_type: u16) -> Result<u16, String> {
    let mut id: u16 = 0;
    let mut err = err_buf();
    if unsafe { orct2_host_ride_create(ride_type, &mut id, err.as_mut_ptr(), ERR_BUF_LEN) } {
        Ok(id)
    } else {
        Err(err_to_string(&err))
    }
}

/// Places one piece at the cursor and advances it. Returns the piece cost.
/// Places one piece and advances the cursor. `speed` is the brake/booster
/// speed stored on the element; the game only reads it for brakes, block
/// brakes and boosters (see `pieces::takes_speed`).
pub fn track_place(
    ride_id: u16,
    track_type: u16,
    chain_lift: bool,
    speed: u8,
    cursor: &mut TrackCursor,
) -> Result<i64, String> {
    let mut cost: i64 = 0;
    let mut err = err_buf();
    if unsafe {
        orct2_host_track_place(
            ride_id,
            track_type,
            chain_lift,
            speed,
            cursor,
            &mut cost,
            err.as_mut_ptr(),
            ERR_BUF_LEN,
        )
    } {
        Ok(cost)
    } else {
        Err(err_to_string(&err))
    }
}

/// status: 0 closed, 1 open, 2 testing, 3 simulating.
pub fn ride_set_status(ride_id: u16, status: u8) -> Result<(), String> {
    let mut err = err_buf();
    if unsafe { orct2_host_ride_set_status(ride_id, status, err.as_mut_ptr(), ERR_BUF_LEN) } {
        Ok(())
    } else {
        Err(err_to_string(&err))
    }
}

/// Names a ride and sets colour scheme zero, which is the scheme all
/// agent-placed track uses. The C++ side validates the ordinary game actions
/// before applying any of them.
pub fn ride_style(
    ride_id: u16,
    name: &str,
    track_color: &str,
    rail_color: &str,
    support_color: &str,
) -> Result<(), String> {
    let name = CString::new(name).map_err(|_| "ride name contains a NUL byte")?;
    let track_color = CString::new(track_color).map_err(|_| "track color contains a NUL byte")?;
    let rail_color = CString::new(rail_color).map_err(|_| "rail color contains a NUL byte")?;
    let support_color =
        CString::new(support_color).map_err(|_| "support color contains a NUL byte")?;
    let mut err = err_buf();
    if unsafe {
        orct2_host_ride_style(
            ride_id,
            name.as_ptr(),
            track_color.as_ptr(),
            rail_color.as_ptr(),
            support_color.as_ptr(),
            err.as_mut_ptr(),
            ERR_BUF_LEN,
        )
    } {
        Ok(())
    } else {
        Err(err_to_string(&err))
    }
}

pub fn ride_detail(ride_id: u16) -> Option<RideDetail> {
    let mut detail = RideDetail::default();
    unsafe { orct2_host_ride_detail(ride_id, &mut detail) }.then_some(detail)
}

/// Places a ride entrance (or exit) on the tile adjacent to a station.
pub fn entrance_place(
    ride_id: u16,
    tile_x: i32,
    tile_y: i32,
    direction: u8,
    is_exit: bool,
) -> Result<(), String> {
    let mut err = err_buf();
    if unsafe {
        orct2_host_entrance_place(
            ride_id,
            tile_x,
            tile_y,
            direction,
            is_exit,
            err.as_mut_ptr(),
            ERR_BUF_LEN,
        )
    } {
        Ok(())
    } else {
        Err(err_to_string(&err))
    }
}

/// Whether sprite data is loaded. False under `eval --no-graphics` (no RCT2
/// assets): screenshots cannot render, so image tools must not be offered.
pub fn graphics_available() -> bool {
    unsafe { orct2_host_graphics_available() }
}

/// Renders a park screenshot to `path` (PNG). With `fit_track` the view is
/// cropped to the bounding box of all track in the park (falls back to the
/// full map when no track exists). With `xray` terrain and supports are
/// hidden so every placed piece is visible, including tunnelled track.
pub fn capture(path: &str, zoom: i32, rotation: u8, fit_track: bool, xray: bool) -> bool {
    match CString::new(path) {
        Ok(cpath) => unsafe { orct2_host_capture(cpath.as_ptr(), zoom, rotation, fit_track, xray) },
        Err(_) => false,
    }
}

/// Tile bbox + z range of all track elements in the park; None when trackless.
pub fn track_bounds() -> Option<TrackBounds> {
    let mut bounds = TrackBounds::default();
    unsafe { orct2_host_track_bounds(&mut bounds) }.then_some(bounds)
}

/// Writes the park to `path` as a .park save.
pub fn save_park(path: &str) -> bool {
    let Ok(cstr) = CString::new(path) else {
        return false;
    };
    unsafe { orct2_host_save_park(cstr.as_ptr()) }
}

/// Replaces the live game state with a previously saved .park snapshot.
pub fn load_park(path: &str) -> bool {
    let Ok(cstr) = CString::new(path) else {
        return false;
    };
    unsafe { orct2_host_load_park(cstr.as_ptr()) }
}

/// Frame size for the current track, so a caller can size one buffer and reuse
/// it for every frame of a replay.
pub fn capture_size(zoom: i32, rotation: u8, fit_track: bool) -> Option<(u32, u32)> {
    let (mut w, mut h) = (0u32, 0u32);
    unsafe { orct2_host_capture_size(zoom, rotation, fit_track, &mut w, &mut h) }.then_some((w, h))
}

/// Renders one frame as RGBA into `buf`, returning the bytes written. Zero
/// means the render failed or the buffer was too small.
pub fn capture_frame(zoom: i32, rotation: u8, fit_track: bool, buf: &mut [u8]) -> usize {
    unsafe { orct2_host_capture_frame(zoom, rotation, fit_track, buf.as_mut_ptr(), buf.len()) }
}

/// `Vehicle::Status` of the ride's lead train: 0 movingToEndOfStation,
/// 1 waitingForPassengers, 2 waitingToDepart, 3 departing, 4 travelling,
/// 5 arriving, 6 unloadingPassengers.
pub fn vehicle_status(ride_id: u16) -> Option<u8> {
    let mut status = 0u8;
    unsafe { orct2_host_vehicle_status(ride_id, &mut status) }.then_some(status)
}

/// Walks `ride_id`'s circuit the way its trains do. None when the ride has no
/// station to start from, which is also true of a ride that was never built.
pub fn circuit_stats(ride_id: u16) -> Option<CircuitStats> {
    let mut stats = CircuitStats::default();
    unsafe { orct2_host_circuit_stats(ride_id, &mut stats) }.then_some(stats)
}

/// Advances the game simulation by `n` ticks (40 ticks = 1 game second).
pub fn run_ticks(n: u32) {
    unsafe { orct2_host_run_ticks(n) };
}

/// Validates a piece at the cursor without placing it or spending money.
pub fn track_query(ride_id: u16, track_type: u16, cursor: &TrackCursor) -> Result<(), String> {
    let mut err = err_buf();
    if unsafe { orct2_host_track_query(ride_id, track_type, cursor, err.as_mut_ptr(), ERR_BUF_LEN) }
    {
        Ok(())
    } else {
        Err(err_to_string(&err))
    }
}

/// Removes a ride and all of its track.
pub fn ride_demolish(ride_id: u16) -> Result<(), String> {
    let mut err = err_buf();
    if unsafe { orct2_host_ride_demolish(ride_id, err.as_mut_ptr(), ERR_BUF_LEN) } {
        Ok(())
    } else {
        Err(err_to_string(&err))
    }
}

/// Forces the ratings state machine to completion for one ride.
pub fn update_ratings(ride_id: u16) {
    unsafe { orct2_host_update_ratings(ride_id) };
}

/// Removes the track piece whose placement origin was `origin`.
pub fn track_remove(ride_id: u16, track_type: u16, origin: &TrackCursor) -> Result<(), String> {
    let mut err = err_buf();
    if unsafe {
        orct2_host_track_remove(ride_id, track_type, origin, err.as_mut_ptr(), ERR_BUF_LEN)
    } {
        Ok(())
    } else {
        Err(err_to_string(&err))
    }
}

/// Pure cursor displacement of a piece for a given input facing (0-3).
pub fn piece_delta(track_type: u16, dir_in: u8) -> Option<TrackCursor> {
    let mut out = TrackCursor {
        x: 0,
        y: 0,
        z: 0,
        direction: 0,
        bank: 0,
        slope: 0,
    };
    unsafe { orct2_host_piece_delta(track_type, dir_in, &mut out) }.then_some(out)
}

/// The stock track design library as a JSON string (see library.rs for the
/// shape). None when the host has no library to give.
pub fn track_library_json() -> Option<String> {
    let ptr = unsafe { orct2_host_track_library_json() };
    if ptr.is_null() {
        return None;
    }
    let json = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { orct2_host_string_free(ptr) };
    Some(json)
}

/// Left/right mirrored counterpart of a track piece (the game's TED mirror
/// table). Symmetric pieces map to themselves.
pub fn track_mirror(track_type: u16) -> u16 {
    unsafe { orct2_host_track_mirror(track_type) }
}

// Link stubs for `cargo test`: the C++ host only exists inside the game
// binary, so the unit-test binary needs these symbols to link. Unit tests
// cover pure logic only (parsing, protocol, formatting); anything that
// touches game state is exercised end-to-end through coasterbench-cli.
#[cfg(test)]
mod test_stubs {
    #![allow(clippy::missing_safety_doc)]
    use std::os::raw::c_char;

    use crate::host::{RideDetail, RideStats, TrackCursor};

    pub unsafe fn orct2_host_ride_count() -> u32 {
        0
    }
    pub unsafe fn orct2_host_ride_stats(_i: u32, _o: *mut RideStats) -> bool {
        false
    }
    pub unsafe fn orct2_host_log(_m: *const c_char) {}
    pub unsafe fn orct2_host_sandbox() -> bool {
        false
    }
    pub unsafe fn orct2_host_surface_z(_x: i32, _y: i32) -> i32 {
        112
    }
    pub unsafe fn orct2_host_ride_create(
        _t: u16,
        _o: *mut u16,
        _e: *mut c_char,
        _l: usize,
    ) -> bool {
        false
    }
    #[allow(clippy::too_many_arguments)] // mirrors the C ABI it stands in for
    pub unsafe fn orct2_host_track_place(
        _r: u16,
        _t: u16,
        _c: bool,
        _s: u8,
        _cur: *mut TrackCursor,
        _cost: *mut i64,
        _e: *mut c_char,
        _l: usize,
    ) -> bool {
        false
    }
    pub unsafe fn orct2_host_ride_set_status(_r: u16, _s: u8, _e: *mut c_char, _l: usize) -> bool {
        false
    }
    #[allow(clippy::too_many_arguments)] // mirrors the C ABI it stands in for
    pub unsafe fn orct2_host_ride_style(
        _r: u16,
        _n: *const c_char,
        _t: *const c_char,
        _a: *const c_char,
        _s: *const c_char,
        _e: *mut c_char,
        _l: usize,
    ) -> bool {
        false
    }
    pub unsafe fn orct2_host_ride_detail(_r: u16, _o: *mut RideDetail) -> bool {
        false
    }
    pub unsafe fn orct2_host_graphics_available() -> bool {
        true
    }
    pub unsafe fn orct2_host_capture(
        _p: *const c_char,
        _z: i32,
        _r: u8,
        _f: bool,
        _u: bool,
    ) -> bool {
        false
    }
    pub unsafe fn orct2_host_track_bounds(_o: *mut crate::host::TrackBounds) -> bool {
        false
    }
    pub unsafe fn orct2_host_circuit_stats(_r: u16, _o: *mut crate::host::CircuitStats) -> bool {
        false
    }
    pub unsafe fn orct2_host_save_park(_p: *const c_char) -> bool {
        false
    }
    pub unsafe fn orct2_host_load_park(_p: *const c_char) -> bool {
        false
    }
    pub unsafe fn orct2_host_capture_frame(
        _z: i32,
        _r: u8,
        _f: bool,
        _o: *mut u8,
        _c: usize,
    ) -> usize {
        0
    }
    pub unsafe fn orct2_host_capture_size(
        _z: i32,
        _r: u8,
        _f: bool,
        _w: *mut u32,
        _h: *mut u32,
    ) -> bool {
        false
    }
    pub unsafe fn orct2_host_vehicle_status(_r: u16, _s: *mut u8) -> bool {
        false
    }
    pub unsafe fn orct2_host_entrance_place(
        _r: u16,
        _x: i32,
        _y: i32,
        _d: u8,
        _ex: bool,
        _e: *mut c_char,
        _l: usize,
    ) -> bool {
        false
    }
    pub unsafe fn orct2_host_run_ticks(_n: u32) {}
    pub unsafe fn orct2_host_track_query(
        _r: u16,
        _t: u16,
        _c: *const TrackCursor,
        _e: *mut c_char,
        _l: usize,
    ) -> bool {
        false
    }
    pub unsafe fn orct2_host_ride_demolish(_r: u16, _e: *mut c_char, _l: usize) -> bool {
        false
    }
    pub unsafe fn orct2_host_update_ratings(_r: u16) {}
    pub unsafe fn orct2_host_track_remove(
        _r: u16,
        _t: u16,
        _o: *const TrackCursor,
        _e: *mut c_char,
        _l: usize,
    ) -> bool {
        false
    }
    pub unsafe fn orct2_host_piece_delta(_t: u16, _d: u8, _cur: *mut TrackCursor) -> bool {
        false
    }
    pub unsafe fn orct2_host_track_library_json() -> *mut c_char {
        std::ptr::null_mut()
    }
    pub unsafe fn orct2_host_string_free(_s: *mut c_char) {}
    pub unsafe fn orct2_host_track_mirror(t: u16) -> u16 {
        t
    }
}
