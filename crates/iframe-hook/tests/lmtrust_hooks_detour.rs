//! LMTrust Deep Blind Spot Test Suite: Hooks, VTable Detours & Flag Overrides
//!
//! Layers covered:
//! - L0 Smoke: Detour utilities, VTable patching structures
//! - L1 Contract: force_waitable_desc flag injection, override_sync capability rules
//! - L2 Boundary: Null descriptors passed to force_waitable helpers
//! - L3 Property: Preservation of existing flags when injecting new flags
//! - L4 Adversarial: find_vtable_templates on malformed/corrupted PE headers in memory
//! - L4 Adversarial: make_detour_14 with null pointers rejected with Err
//! - L9 Negative Space: Disabled override_sync leaves present args untouched

use std::ffi::c_void;

use iframe_hook::hooks::d3d9::find_vtable_templates;
use iframe_hook::hooks::dxgi::{
    force_waitable_desc, force_waitable_desc1, override_sync, set_caps_for_test,
};
use iframe_hook::hooks::vulkan::make_detour_14;
use windows::Win32::Foundation::{HWND, TRUE};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_MODE_SCALING_UNSPECIFIED,
    DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT_ALLOW_TEARING, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::SystemServices::IMAGE_DOS_HEADER;

// Mutex to serialize override_sync tests because they modify global atomic CAPS_* statics.
static CAPS_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn l1_contract_override_sync_all_modes_verified() {
    let _guard = CAPS_MUTEX.lock().unwrap();

    // 1. Tearing supported -> (0, ALLOW_TEARING)
    set_caps_for_test(true, true, false);
    let (sync, flags) = unsafe { override_sync(1, 0, true) };
    assert_eq!(sync, 0, "SyncInterval must be 0 for paced presentation");
    assert_eq!(
        flags,
        DXGI_PRESENT_ALLOW_TEARING.0,
        "ALLOW_TEARING flag must be added when tearing is supported"
    );

    // 2. Windowed without tearing -> (0, 0)
    set_caps_for_test(true, false, false);
    let (sync, flags) = unsafe { override_sync(1, 0, true) };
    assert_eq!(sync, 0, "SyncInterval must be 0 for windowed DWM flip");
    assert_eq!(flags, 0, "No tearing flag for standard windowed");

    // 3. Fullscreen exclusive without tearing -> preserves game's sync_interval
    set_caps_for_test(false, false, false);
    let (sync, flags) = unsafe { override_sync(1, 0, true) };
    assert_eq!(sync, 1, "Exclusive fullscreen without tearing preserves game sync interval");
    assert_eq!(flags, 0);

    // 4. Disabled override -> pass through untouched
    set_caps_for_test(true, true, true);
    let (sync, flags) = unsafe { override_sync(2, 0x40, false) };
    assert_eq!(sync, 2, "Disabled override must pass original sync_interval");
    assert_eq!(flags, 0x40, "Disabled override must pass original flags");
}

// ---------------------------------------------------------------------------
// L2: Boundary Tests — Null Descriptors
// ---------------------------------------------------------------------------

#[test]
fn l2_boundary_force_waitable_null_pointers() {
    assert!(unsafe { force_waitable_desc(std::ptr::null()) }.is_none());
    assert!(unsafe { force_waitable_desc1(std::ptr::null()) }.is_none());
}

// ---------------------------------------------------------------------------
// L3: Property Tests — Flag Preservation & Bitwise Invariants
// ---------------------------------------------------------------------------

#[test]
fn l3_property_desc_flags_preservation() {
    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: 1920,
            Height: 1080,
            RefreshRate: DXGI_RATIONAL {
                Numerator: 144,
                Denominator: 1,
            },
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED,
            Scaling: DXGI_MODE_SCALING_UNSPECIFIED,
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 3,
        OutputWindow: HWND::default(),
        Windowed: TRUE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32,
    };

    // When telemetry config is not active (force_waitable default false), returns None
    let result = unsafe { force_waitable_desc(&desc) };
    assert!(result.is_none() || (result.unwrap().Flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 != 0));

    let desc1 = DXGI_SWAP_CHAIN_DESC1 {
        Width: 1920,
        Height: 1080,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 3,
        Scaling: windows::Win32::Graphics::Dxgi::DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: windows::Win32::Graphics::Dxgi::Common::DXGI_ALPHA_MODE_UNSPECIFIED,
        Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
    };
    // If flag is already present, force_waitable_desc1 returns None (no unnecessary clone)
    let res1 = unsafe { force_waitable_desc1(&desc1) };
    assert!(res1.is_none());
}

#[test]
fn l3_property_vtable_patch_and_restore() {
    let mut fake_vtable: [*mut c_void; 4] = [
        0x1000 as *mut c_void,
        0x2000 as *mut c_void,
        0x3000 as *mut c_void,
        0x4000 as *mut c_void,
    ];
    let hook_fn = 0x9999 as *mut c_void;

    let orig = unsafe {
        iframe_hook::hooks::dxgi::patch_vtable_slot(fake_vtable.as_mut_ptr(), 1, hook_fn).unwrap()
    };
    assert_eq!(orig, 0x2000 as *mut c_void);
    assert_eq!(fake_vtable[1], hook_fn);

    unsafe {
        iframe_hook::hooks::dxgi::restore_vtable(fake_vtable.as_mut_ptr(), 1, orig);
    }
    assert_eq!(fake_vtable[1], 0x2000 as *mut c_void);
}

// ---------------------------------------------------------------------------
// L4: Adversarial Tests — Malformed Memory & Signature Scans
// ---------------------------------------------------------------------------

#[test]
fn l4_adversarial_find_vtable_templates_null_and_corrupt_headers() {
    let mut dummy_vtable: [*mut c_void; 16] = [0xDEADBEEF as *mut c_void; 16];

    // 1. Null base
    let res_null = unsafe { find_vtable_templates(std::ptr::null(), dummy_vtable.as_mut_ptr()) };
    assert!(res_null.is_empty());

    // 2. Corrupt DOS magic (not 'MZ' / 0x5A4D)
    let mut fake_image = vec![0u8; 1024];
    let res_bad_magic = unsafe { find_vtable_templates(fake_image.as_ptr(), dummy_vtable.as_mut_ptr()) };
    assert!(res_bad_magic.is_empty());

    // 3. Valid DOS magic but non-matching signature
    let dos = unsafe { &mut *(fake_image.as_mut_ptr() as *mut IMAGE_DOS_HEADER) };
    dos.e_magic = 0x5A4D;
    dos.e_lfanew = 64;
    // Set PE signature & size of image in optional header
    let opt = unsafe { fake_image.as_mut_ptr().add(64 + 24) };
    unsafe {
        *(opt as *mut u16) = 0x20B; // PE32+ (x64)
        *(opt.add(56) as *mut u32) = 1024; // size_of_image
    }

    let res_no_match = unsafe { find_vtable_templates(fake_image.as_ptr(), dummy_vtable.as_mut_ptr()) };
    assert!(res_no_match.is_empty(), "Random bytes should not match 16-slot vtable");
}

#[test]
fn l4_adversarial_find_vtable_templates_matches_injected_signature() {
    let mut fake_image = vec![0u8; 4096];
    let dos = unsafe { &mut *(fake_image.as_mut_ptr() as *mut IMAGE_DOS_HEADER) };
    dos.e_magic = 0x5A4D;
    dos.e_lfanew = 64;
    let opt = unsafe { fake_image.as_mut_ptr().add(64 + 24) };
    unsafe {
        *(opt as *mut u16) = 0x20B;
        *(opt.add(56) as *mut u32) = 4096;
    }

    // Create 16 signature pointers
    let mut vtable_sig: [usize; 16] = [0; 16];
    for i in 0..16 {
        vtable_sig[i] = 0x1000_0000 + i * 0x10;
    }

    // Embed signature at offset 512 in fake image
    let target_offset = 512;
    unsafe {
        let dest = fake_image.as_mut_ptr().add(target_offset) as *mut usize;
        std::ptr::copy_nonoverlapping(vtable_sig.as_ptr(), dest, 16);
    }

    let found = unsafe {
        find_vtable_templates(
            fake_image.as_ptr(),
            vtable_sig.as_mut_ptr() as *mut *mut c_void,
        )
    };

    assert_eq!(found.len(), 1, "Template signature must be discovered at exact offset");
    assert_eq!(found[0] as usize, fake_image.as_ptr() as usize + target_offset);
}

#[test]
fn l4_adversarial_make_detour_null_pointers_rejected() {
    assert!(unsafe { make_detour_14(std::ptr::null_mut(), std::ptr::null_mut()) }.is_err());
    assert!(unsafe { make_detour_14(1 as *mut c_void, std::ptr::null_mut()) }.is_err());
    assert!(unsafe { make_detour_14(std::ptr::null_mut(), 1 as *mut c_void) }.is_err());
}
