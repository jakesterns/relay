//! Decoded (or captured) NV12 textures to NDI®, without stalling the thread
//! that presents them.
//!
//! The vcam sink maps its staging copy straight away, which waits for the GPU
//! to finish the copy. Here there are two staging textures: frame N is copied
//! into one while frame N-1, copied a frame ago and long finished, is mapped
//! from the other with `D3D11_MAP_FLAG_DO_NOT_WAIT`. If the GPU is somehow
//! still busy with it, that frame is dropped rather than waited for. NDI
//! output is one frame behind the window, which no NDI receiver can see.

#![allow(unsafe_code)] // D3D11 staging copy + map; SAFETY notes inline

use std::sync::Arc;

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;

use super::convert::{self, FrameRate};
use super::NdiOutput;
use crate::decode::mf::DecodedFrame;

struct Slot {
    tex: ID3D11Texture2D,
    /// Texture size (the decoder's aligned size).
    tex_w: u32,
    tex_h: u32,
}

/// What a staged copy holds, waiting to be mapped on the next frame.
#[derive(Clone, Copy)]
struct Staged {
    slot: usize,
    width: u32,
    height: u32,
    timecode: i64,
}

/// The render (or capture) thread's half of NDI video.
pub struct VideoProducer {
    out: Arc<NdiOutput>,
    slots: [Option<Slot>; 2],
    next: usize,
    staged: Option<Staged>,
    rate: FrameRate,
}

impl VideoProducer {
    pub fn new(out: Arc<NdiOutput>, default_fps: u32) -> Self {
        Self { out, slots: [None, None], next: 0, staged: None, rate: FrameRate::new(default_fps) }
    }

    pub fn output(&self) -> &Arc<NdiOutput> {
        &self.out
    }

    /// Forget every GPU resource, after a failure: the next frame (once
    /// output is turned on again) starts from scratch.
    pub fn reset(&mut self) {
        self.slots = [None, None];
        self.staged = None;
    }

    /// Offer one frame. A no-op (one atomic load) while NDI output is off.
    /// Never waits on the GPU or the NDI worker; errors are D3D failures the
    /// caller logs once before dropping the producer.
    pub fn push(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        frame: &DecodedFrame,
    ) -> Result<()> {
        if !self.out.video.is_active() {
            // Forget the staged frame: it would be stale when output resumes.
            self.staged = None;
            return Ok(());
        }
        let rate = self.rate.push(frame.pts_100ns);

        // 1. Hand last frame's copy to the worker, if the GPU has finished it.
        if let Some(st) = self.staged.take() {
            self.read_back(context, st, rate)?;
        }

        // 2. Copy this frame into the other slot, to be read next frame.
        let slot = self.next;
        self.ensure_slot(device, frame, slot)?;
        let s = self.slots[slot].as_ref().expect("ensured");
        // SAFETY: live textures on this thread's immediate context; the
        // staging texture was created to match the source's size and format.
        unsafe {
            context.CopySubresourceRegion(
                &s.tex,
                0,
                0,
                0,
                0,
                &frame.texture,
                frame.subresource,
                None,
            );
        }
        self.staged = Some(Staged {
            slot,
            width: frame.width,
            height: frame.height,
            timecode: self.out.now_ticks(),
        });
        self.next ^= 1;
        Ok(())
    }

    fn ensure_slot(
        &mut self,
        device: &ID3D11Device,
        frame: &DecodedFrame,
        slot: usize,
    ) -> Result<()> {
        // SAFETY: GetDesc on a live texture; CreateTexture2D with a desc that
        // mirrors it as a CPU-readable staging copy.
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            frame.texture.GetDesc(&mut desc);
            if self.slots[slot].as_ref().map(|s| (s.tex_w, s.tex_h))
                == Some((desc.Width, desc.Height))
            {
                return Ok(());
            }
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: desc.SampleDesc,
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex = None;
            device.CreateTexture2D(&staging_desc, None, Some(&mut tex))?;
            self.slots[slot] = Some(Slot {
                tex: tex.context("NDI staging texture")?,
                tex_w: desc.Width,
                tex_h: desc.Height,
            });
        }
        Ok(())
    }

    fn read_back(
        &mut self,
        context: &ID3D11DeviceContext,
        st: Staged,
        rate: (i32, i32),
    ) -> Result<()> {
        let Some(s) = self.slots[st.slot].as_ref() else { return Ok(()) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: a staging texture of ours; DO_NOT_WAIT returns at once.
        let map = unsafe {
            context.Map(
                &s.tex,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            )
        };
        match map {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                // Never wait: count it as a drop and move on.
                self.out.video.try_with(|tx| {
                    tx.stats.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                });
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        let pitch = mapped.RowPitch as usize;
        let tex_h = s.tex_h as usize;
        // SAFETY: the mapped NV12 staging texture is `tex_h` rows of Y then
        // `tex_h / 2` rows of UV at the same pitch, valid until Unmap.
        let (y, uv) = unsafe {
            let base = mapped.pData as *const u8;
            (
                std::slice::from_raw_parts(base, pitch * tex_h),
                std::slice::from_raw_parts(base.add(pitch * tex_h), pitch * (tex_h / 2)),
            )
        };
        let (w, h) = (st.width.min(s.tex_w), st.height.min(s.tex_h));
        self.out.video.try_with(|tx| {
            let Some(mut b) = tx.buffer() else { return };
            match convert::pack_nv12(y, uv, pitch, w as usize, h as usize, &mut b.data) {
                Some(stride) => {
                    b.width = w;
                    b.height = h;
                    b.stride = stride as u32;
                    b.rate = rate;
                    b.timecode = st.timecode;
                    tx.send(b);
                }
                None => tx.give_back(b),
            }
        });
        // SAFETY: balances the Map above.
        unsafe { context.Unmap(&s.tex, 0) };
        Ok(())
    }
}
