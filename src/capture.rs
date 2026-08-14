//! DXGI Desktop Duplication screen capture - region-level GPU crop + merged staging
//!
//! Core optimizations:
//! 1. CopySubresourceRegion + D3D11_BOX crops only the needed small areas (readback cut by 99%)
//! 2. All regions are packed into a single staging texture, only 1 Map/Unmap per frame (driver calls cut by 92%)
//! 3. When the desktop is unchanged (AcquireNextFrame times out) the frame is skipped entirely:
//!    zero readback, zero rendering

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_DESC, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter1, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ENUM_MODES, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

use crate::error::{AppError, AppResult};

const DXGI_ERROR_WAIT_TIMEOUT: i32 = 0x887A0027u32 as i32;
const DXGI_ERROR_ACCESS_LOST: i32 = 0x887A0026u32 as i32;
const D3D11_SDK_VERSION: u32 = 7;

/// Source region to capture (absolute screen coordinates)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Capture result of one region (borrows the internal reusable buffer, zero alloc)
pub struct RegionFrame<'a> {
    /// BGRA pixels, width = w, height = h, row pitch = w*4 (no padding)
    pub data: &'a [u8],
    pub w: u32,
    pub h: u32,
}

/// Per-region CPU reusable buffer + offset inside the merged staging texture
struct RegionSlot {
    cpu: Vec<u8>,
    w: u32,
    h: u32,
    /// X offset (pixels) inside the merged staging texture
    x_offset: u32,
}

/// DXGI Desktop Duplication capturer (region-level GPU crop + merged staging)
pub struct DxgiCapture {
    duplication: IDXGIOutputDuplication,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    output1: IDXGIOutput1,
    pub width: u32,
    pub height: u32,
    rebuild_failures: u32,
    /// Merged staging textures, ping-pong pair (see PIPELINE comment in acquire_regions)
    staging: Vec<Option<ID3D11Texture2D>>,
    staging_w: u32,
    staging_h: u32,
    /// Successfully acquired (copied) frame counter; its parity selects the write/read buffer
    acquired_seq: u64,
    /// Per-region CPU buffers
    slots: Vec<RegionSlot>,
    /// DWM present timestamp (QPC ticks) of the last acquired frame; None before the first frame
    pub last_present_tick: Option<i64>,
    /// Time spent inside AcquireNextFrame waiting for a new desktop frame (ms)
    pub last_wait_ms: f64,
    /// Time spent in GPU copy + readback after the frame arrived (ms)
    pub last_readback_ms: f64,
}

impl DxgiCapture {
    pub fn new() -> AppResult<Self> {
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1::<IDXGIFactory1>()
                    .map_err(|e| AppError::windows("CreateDXGIFactory1", e))?;

            let adapter: IDXGIAdapter1 = factory
                .EnumAdapters1(0)
                .map_err(|e| AppError::windows("EnumAdapters1", e))?;

            let output: IDXGIOutput = adapter
                .EnumOutputs(0)
                .map_err(|e| AppError::windows("EnumOutputs", e))?;

            let out_desc = output
                .GetDesc()
                .map_err(|e| AppError::windows("output GetDesc", e))?;
            let rc = out_desc.DesktopCoordinates;
            let width = (rc.right - rc.left) as u32;
            let height = (rc.bottom - rc.top) as u32;
            log::info!("DDA output: {}x{} @ ({},{})", width, height, rc.left, rc.top);

            // Log the monitor's max refresh rate (caps how fast DWM can compose new
            // desktop frames: DDA can never exceed the desktop composition rate)
            {
                let mut num_modes: u32 = 0;
                let _ = output.GetDisplayModeList(
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ENUM_MODES(0),
                    &mut num_modes,
                    None,
                );
                let mut modes = vec![DXGI_MODE_DESC::default(); num_modes as usize];
                if num_modes > 0
                    && output
                        .GetDisplayModeList(
                            DXGI_FORMAT_B8G8R8A8_UNORM,
                            DXGI_ENUM_MODES(0),
                            &mut num_modes,
                            Some(modes.as_mut_ptr()),
                        )
                        .is_ok()
                {
                    let mut best: Option<DXGI_RATIONAL> = None;
                    for m in &modes {
                        if m.Width != width || m.Height != height || m.RefreshRate.Denominator == 0 {
                            continue;
                        }
                        let better = match best {
                            None => true,
                            Some(b) => {
                                m.RefreshRate.Numerator as u64 * b.Denominator as u64
                                    > b.Numerator as u64 * m.RefreshRate.Denominator as u64
                            }
                        };
                        if better {
                            best = Some(m.RefreshRate);
                        }
                    }
                    if let Some(r) = best {
                        let hz = r.Numerator as f64 / r.Denominator as f64;
                        log::info!(
                            "Monitor max refresh rate: {:.2}Hz (DDA ceiling)",
                            hz
                        );
                    }
                }
            }

            let output1: IDXGIOutput1 = output
                .cast::<IDXGIOutput1>()
                .map_err(|e| AppError::windows("cast IDXGIOutput1", e))?;

            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            let create_result = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            );
            let (device, context) = match create_result {
                Ok(()) => {
                    let d = device.ok_or_else(|| AppError::other("D3D11 device is null"))?;
                    let c = context.ok_or_else(|| AppError::other("D3D11 context is null"))?;
                    (d, c)
                }
                Err(e) => {
                    log::warn!("default adapter D3D11 creation failed ({}), falling back to enumerated adapter", e);
                    let mut device: Option<ID3D11Device> = None;
                    let mut context: Option<ID3D11DeviceContext> = None;
                    // MSDN: when a non-null pAdapter is passed, DriverType must be
                    // D3D_DRIVER_TYPE_UNKNOWN, otherwise the call always fails
                    // (old code used HARDWARE, making the fallback branch dead code)
                    D3D11CreateDevice(
                        &adapter,
                        D3D_DRIVER_TYPE_UNKNOWN,
                        None,
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        None,
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut context),
                    )
                    .map_err(|e| AppError::windows("D3D11CreateDevice(fallback)", e))?;
                    let d = device.ok_or_else(|| AppError::other("D3D11 device is null"))?;
                    let c = context.ok_or_else(|| AppError::other("D3D11 context is null"))?;
                    (d, c)
                }
            };

            let duplication = output1
                .DuplicateOutput(&device)
                .map_err(|e| AppError::windows("DuplicateOutput", e))?;
            log::info!("DXGI Output Duplication established (merged staging mode)");

            Ok(Self {
                duplication,
                context,
                device,
                output1,
                width,
                height,
                rebuild_failures: 0,
                staging: Vec::new(),
                staging_w: 0,
                staging_h: 0,
                acquired_seq: 0,
                slots: Vec::new(),
                last_present_tick: None,
                last_wait_ms: 0.0,
                last_readback_ms: 0.0,
            })
        }
    }

    /// Capture the given source regions of one frame.
    /// Returns Ok(None): desktop unchanged, readback skipped.
    pub fn acquire_regions(
        &mut self,
        timeout_ms: u32,
        regions: &[RegionRect],
    ) -> AppResult<Option<Vec<RegionFrame<'_>>>> {
        unsafe {
            // No regions to capture (UI-only config): skip early - the old code would
            // crash on staging_tex .unwrap(); early return also saves one useless
            // AcquireNextFrame/ReleaseFrame
            if regions.is_empty() {
                return Ok(None);
            }

            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            let t_wait = std::time::Instant::now();
            let acquire_result =
                self.duplication
                    .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource);

            match acquire_result {
                Ok(()) => {}
                Err(e) => {
                    let hresult = e.code().0;
                    if hresult == DXGI_ERROR_WAIT_TIMEOUT {
                        return Ok(None);
                    }
                    if hresult == DXGI_ERROR_ACCESS_LOST {
                        log::warn!("DDA session lost (ACCESS_LOST), trying to rebuild...");
                        self.rebuild_failures += 1;
                        // Back off: an exclusive-fullscreen switch can take a moment; rebuilding
                        // in a tight loop at frame rate would spam the log and hammer DWM.
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        match self.rebuild_duplication() {
                            Ok(()) => return Ok(None),
                            Err(re) => {
                                // Swallow the error and keep retrying: while the game stays in
                                // exclusive fullscreen the rebuild can legitimately fail for a
                                // long time, and the main loop must NOT reach its consecutive
                                // error limit and kill the program (it recovers on its own once
                                // the game switches back to windowed mode).
                                log::error!("rebuilding DDA session failed: {}", re);
                                if self.rebuild_failures.is_multiple_of(10) {
                                    log::error!("DDA rebuild failed {} times consecutively", self.rebuild_failures);
                                }
                                std::thread::sleep(std::time::Duration::from_secs(1));
                                return Ok(None);
                            }
                        }
                    }
                    log::warn!("AcquireNextFrame failed HRESULT=0x{:08X}", hresult as u32);
                    return Err(AppError::windows("AcquireNextFrame", e));
                }
            }

            let resource = resource.ok_or_else(|| AppError::other("acquired resource is null"))?;
            self.last_wait_ms = t_wait.elapsed().as_secs_f64() * 1000.0;
            let t_readback = std::time::Instant::now();
            // DWM present timestamp of this frame: its delta from the previous frame is the
            // source (composition) frame interval, independent of our own loop pacing
            let t = frame_info.LastPresentTime;
            self.last_present_tick = if t != 0 { Some(t) } else { None };
            let src_texture: ID3D11Texture2D = resource
                .cast::<ID3D11Texture2D>()
                .map_err(|e| AppError::windows("cast frame to Texture2D", e))?;

            // Compute packed layout + ensure the staging textures are big enough
            self.ensure_staging(regions)?;

            // Pipelined readback: copy the current DDA frame into write_idx, then read
            // the frame copied on the PREVIOUS acquire call from read_idx. Map blocks
            // until the GPU finishes the pending copy; reading a copy submitted one
            // iteration ago gives the GPU a whole frame interval to finish it, instead
            // of stalling right after submission (a game saturating the GPU made the
            // stall ~30ms). Copy/readback alternate via acquired_seq parity.
            let w_idx = (self.acquired_seq % 2) as usize;
            let r_idx = ((self.acquired_seq + 1) % 2) as usize;

            // GPU-side crop: CopySubresourceRegion per region to different X offsets
            let tex = self.staging[w_idx].as_ref().unwrap();
            for (i, r) in regions.iter().enumerate() {
                let x_off = self.slots[i].x_offset;
                // Clamp to the desktop bounds: boxes outside the screen are illegal
                // parameters (old code passed them through directly)
                let right = (r.x + r.w).min(self.width);
                let bottom = (r.y + r.h).min(self.height);
                let left = r.x.min(right);
                let top = r.y.min(bottom);
                if left >= right || top >= bottom {
                    log::warn!("region {} is fully outside the screen, copy skipped", i);
                    continue;
                }
                let src_box = D3D11_BOX {
                    left,
                    top,
                    front: 0,
                    right,
                    bottom,
                    back: 1,
                };
                self.context.CopySubresourceRegion(
                    tex, 0,
                    x_off, 0, 0, // dst offset: horizontal layout
                    &src_texture, 0,
                    Some(&src_box as *const D3D11_BOX),
                );
            }

            // First frame (or right after a staging rebuild): nothing was copied before,
            // so there is nothing to read back yet; skip readback and return None, keeping
            // the loop shape identical. acquired_seq is incremented before that check so
            // the NEXT call reads the copy just submitted.
            self.acquired_seq += 1;
            if self.acquired_seq <= 1 {
                let _ = self.duplication.ReleaseFrame();
                return Ok(None);
            }

            // Single Map readback for all regions (driver calls reduced from N*2 to 2)
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            if let Err(e) = self
                .context
                .Map(self.staging[r_idx].as_ref().unwrap(), 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            {
                // Release the current frame first, otherwise the next AcquireNextFrame
                // is rejected with E_INVALIDARG, and 30 consecutive errors would trigger
                // the main loop "safe exit"
                let _ = self.duplication.ReleaseFrame();
                return Err(AppError::windows("Map merged staging", e));
            }

            let src_ptr = mapped.pData as *const u8;
            let row_pitch = mapped.RowPitch as usize;
            for i in 0..regions.len() {
                let slot = &mut self.slots[i];
                let row_bytes = slot.w as usize * 4;
                let base_x_byte = slot.x_offset as usize * 4;
                for row in 0..slot.h as usize {
                    std::ptr::copy_nonoverlapping(
                        src_ptr.add(row * row_pitch + base_x_byte),
                        slot.cpu.as_mut_ptr().add(row * row_bytes),
                        row_bytes,
                    );
                }
            }
            self.context.Unmap(self.staging[r_idx].as_ref().unwrap(), 0);
            self.last_readback_ms = t_readback.elapsed().as_secs_f64() * 1000.0;

            self.duplication
                .ReleaseFrame()
                .map_err(|e| AppError::windows("ReleaseFrame", e))?;

            let frames: Vec<RegionFrame<'_>> = self
                .slots
                .iter()
                .map(|s| RegionFrame { data: &s.cpu, w: s.w, h: s.h })
                .collect();

            Ok(Some(frames))
        }
    }

    /// Ensure the merged staging textures are big enough for all regions (horizontal
    /// layout). Two textures for pipelined readback; rebuilt together (one is not
    /// useful without the other), which also resets the pipeline sequence.
    fn ensure_staging(&mut self, regions: &[RegionRect]) -> AppResult<()> {
        // Compute the packed layout
        let mut total_w: u32 = 0;
        let mut max_h: u32 = 0;
        for r in regions {
            total_w += r.w;
            max_h = max_h.max(r.h);
        }
        if total_w == 0 || max_h == 0 {
            return Ok(());
        }

        unsafe {
            // Rebuild the staging textures (when the size is insufficient)
            if self.staging_w < total_w || self.staging_h < max_h {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: total_w,
                    Height: max_h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut tex0: Option<ID3D11Texture2D> = None;
                let mut tex1: Option<ID3D11Texture2D> = None;
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut tex0))
                    .map_err(|e| AppError::windows("CreateTexture2D merged staging", e))?;
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut tex1))
                    .map_err(|e| AppError::windows("CreateTexture2D merged staging (2)", e))?;
                self.staging = vec![tex0, tex1];
                self.staging_w = total_w;
                self.staging_h = max_h;
                // Pipeline restart: the next acquire re-seeds buffer 0, and the one after
                // reads it; without this, the first Map after a resizing rebuild could
                // read an empty texture.
                self.acquired_seq = 0;
                log::info!("merged staging rebuilt: {}x{} (ping-pong)", total_w, max_h);
            }

            // Sync slots
            self.slots.truncate(regions.len());
            while self.slots.len() < regions.len() {
                self.slots.push(RegionSlot { cpu: Vec::new(), w: 0, h: 0, x_offset: 0 });
            }
            let mut x_off: u32 = 0;
            for (i, r) in regions.iter().enumerate() {
                let slot = &mut self.slots[i];
                slot.x_offset = x_off;
                if slot.w != r.w || slot.h != r.h {
                    slot.w = r.w;
                    slot.h = r.h;
                    slot.cpu = vec![0u8; (r.w * r.h * 4) as usize];
                }
                x_off += r.w;
            }
        }
        Ok(())
    }

    /// Rebuild the DDA session from scratch.
    ///
    /// IMPORTANT: reusing the stale `self.output1`/`self.device` handles after an
    /// exclusive-fullscreen switch does NOT recover — DWM tore down the desktop
    /// composition surface, so a fresh DuplicateOutput on the old handle keeps
    /// returning ACCESS_LOST forever (observed: rebuild "succeeds" then the very
    /// next AcquireNextFrame fails again, infinite loop at frame rate).
    /// Only a full re-enumeration (factory -> adapter -> output -> device) recovers.
    fn rebuild_duplication(&mut self) -> AppResult<()> {
        log::info!("trying to rebuild the DDA duplication session...");
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1::<IDXGIFactory1>()
                    .map_err(|e| AppError::windows("rebuild: CreateDXGIFactory1", e))?;
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapters1(0)
                .map_err(|e| AppError::windows("rebuild: EnumAdapters1", e))?;
            let output: IDXGIOutput = adapter
                .EnumOutputs(0)
                .map_err(|e| AppError::windows("rebuild: EnumOutputs", e))?;
            let out_desc = output
                .GetDesc()
                .map_err(|e| AppError::windows("rebuild: output GetDesc", e))?;
            let rc = out_desc.DesktopCoordinates;
            let width = (rc.right - rc.left) as u32;
            let height = (rc.bottom - rc.top) as u32;

            let output1: IDXGIOutput1 = output
                .cast::<IDXGIOutput1>()
                .map_err(|e| AppError::windows("rebuild: cast IDXGIOutput1", e))?;

            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| AppError::windows("rebuild: D3D11CreateDevice", e))?;
            let device = device.ok_or_else(|| AppError::other("rebuild: device is null"))?;
            let context = context.ok_or_else(|| AppError::other("rebuild: context is null"))?;

            let duplication = output1
                .DuplicateOutput(&device)
                .map_err(|e| AppError::windows("rebuild: DuplicateOutput", e))?;

            self.duplication = duplication;
            self.context = context;
            self.device = device;
            self.output1 = output1;

            // Clear the staging cache (pipeline restarts on the next acquire)
            self.staging = Vec::new();
            self.staging_w = 0;
            self.staging_h = 0;
            self.slots.clear();
            self.acquired_seq = 0;
            self.last_present_tick = None;

            let old_w = self.width;
            let old_h = self.height;
            self.width = width;
            self.height = height;

            log::info!("DDA session rebuilt: {}x{} (was {}x{})", width, height, old_w, old_h);
        }
        Ok(())
    }
}
