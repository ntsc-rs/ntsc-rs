//! Derived from GStreamer's gst-plugins-good imagefreeze element.
//!
//! This differs from GStreamer's imagefreeze in that it doesn't have `is-live` or `allow-replace`, and allows a
//! framerate to be directly specified (rather than relying on a caps filter downstream, which I've found quite buggy).
//!
//! Copyright (C) 2005 Edward Hervey <bilboed@bilboed.com>
//! Copyright (C) 2010 Sebastian Dröge <sebastian.droege@collabora.co.uk>
//! Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//! Copyright (C) 2026 ntsc-rs contributors
//!
//! SPDX-License-Identifier: LGPL-2.0-or-later
//!

use std::sync::{LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gstreamer as gst;
use gstreamer::GenericFormattedValue;

fn default_framerate() -> gst::Fraction {
    gst::Fraction::new(30, 1)
}

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "ntscrsimagefreeze",
        gst::DebugColorFlags::empty(),
        Some("ntsc-rs still frame stream generator"),
    )
});

#[derive(Debug)]
struct State {
    framerate: gst::Fraction,
    duration: Option<gst::ClockTime>,

    buffer: Option<gst::Buffer>,
    buffer_caps: Option<gst::Caps>,
    current_caps: Option<gst::Caps>,

    segment: gst::FormattedSegment<gst::ClockTime>,
    need_segment: bool,
    seqnum: Option<gst::Seqnum>,
    offset: u64,

    flushing: bool,
    direct_eos: bool,
}

impl State {
    fn new() -> Self {
        let mut state = Self {
            framerate: default_framerate(),
            duration: None,
            buffer: None,
            buffer_caps: None,
            current_caps: None,
            segment: gst::FormattedSegment::new(),
            need_segment: true,
            seqnum: None,
            offset: 0,
            flushing: true,
            direct_eos: false,
        };
        state.reset_segment();
        state
    }

    fn reset_segment(&mut self) {
        self.segment.reset();
        self.segment.set_stop(self.duration);
        self.segment.set_duration(self.duration);
        self.need_segment = true;
        self.offset = 0;
        self.seqnum = None;
    }

    fn reset_runtime(&mut self) {
        self.buffer = None;
        self.buffer_caps = None;
        self.current_caps = None;
        self.reset_segment();
        self.flushing = true;
        self.direct_eos = false;
    }
}

#[derive(Debug)]
pub struct ImageFreeze {
    sinkpad: gst::Pad,
    srcpad: gst::Pad,
    state: Mutex<State>,
}

impl ImageFreeze {
    fn output_caps(input_caps: &gst::Caps, framerate: gst::Fraction) -> gst::Caps {
        let mut caps = input_caps.clone();
        let caps_mut = caps.make_mut();
        for idx in 0..caps_mut.size() {
            caps_mut
                .structure_mut(idx)
                .expect("caps structure disappeared")
                .set("framerate", framerate);
        }
        caps
    }

    fn remove_framerate(caps: &mut gst::CapsRef) {
        for idx in 0..caps.size() {
            let structure = caps.structure_mut(idx).expect("caps structure disappeared");
            structure.remove_field("framerate");
            structure.set(
                "framerate",
                gst::FractionRange::new(gst::Fraction::new(0, 1), gst::Fraction::new(i32::MAX, 1)),
            );
        }
    }

    fn query_caps(&self, pad: &gst::Pad, filter: Option<&gst::CapsRef>) -> gst::Caps {
        let otherpad = if pad == &self.srcpad {
            &self.sinkpad
        } else {
            &self.srcpad
        };

        let mut peer_filter = filter.map(ToOwned::to_owned);
        if let Some(filter) = peer_filter.as_mut() {
            Self::remove_framerate(filter.make_mut());
        }

        let template_caps = pad.pad_template_caps();
        let mut caps = otherpad
            .peer_query_caps(peer_filter.as_ref())
            .intersect_with_mode(&template_caps, gst::CapsIntersectMode::First);

        if pad == &self.srcpad {
            let framerate = self.state.lock().unwrap().framerate;
            let caps_mut = caps.make_mut();
            for idx in 0..caps_mut.size() {
                caps_mut
                    .structure_mut(idx)
                    .expect("caps structure disappeared")
                    .set("framerate", framerate);
            }
        } else {
            Self::remove_framerate(caps.make_mut());
        }

        if let Some(filter) = filter {
            caps = filter.intersect_with_mode(&caps, gst::CapsIntersectMode::First);
        }

        gst::log!(CAT, obj = pad, "Returning caps {caps:?}");
        caps
    }

    fn frames_for_time(time: gst::ClockTime, framerate: gst::Fraction) -> u64 {
        time.nseconds()
            .mul_div_floor(
                framerate.numer() as u64,
                framerate.denom() as u64 * gst::ClockTime::SECOND.nseconds(),
            )
            .unwrap_or(u64::MAX)
    }

    fn time_for_frame(frame: u64, framerate: gst::Fraction) -> gst::ClockTime {
        gst::ClockTime::SECOND
            .mul_div_floor(
                frame.saturating_mul(framerate.denom() as u64),
                framerate.numer() as u64,
            )
            .unwrap_or(gst::ClockTime::MAX)
    }

    fn convert_value(
        &self,
        source: gst::GenericFormattedValue,
        destination_format: gst::Format,
    ) -> Option<gst::GenericFormattedValue> {
        if source.format() == destination_format {
            return Some(source);
        }
        if source.value() == -1 {
            return Some(gst::GenericFormattedValue::new(destination_format, -1));
        }

        let framerate = self.state.lock().unwrap().framerate;
        let value = match (source.format(), destination_format) {
            (gst::Format::Default, gst::Format::Time) => {
                Self::time_for_frame(source.value().try_into().ok()?, framerate)
                    .nseconds()
                    .try_into()
                    .ok()?
            }
            (gst::Format::Time, gst::Format::Default) => {
                let time = gst::ClockTime::from_nseconds(source.value().try_into().ok()?);
                Self::frames_for_time(time, framerate).try_into().ok()?
            }
            _ => return None,
        };

        Some(gst::GenericFormattedValue::new(destination_format, value))
    }

    fn start_src_task(&self) -> Result<(), glib::BoolError> {
        let imp = self.ref_counted();
        self.srcpad.start_task(move || imp.src_loop())
    }

    fn sink_chain(
        &self,
        pad: &gst::Pad,
        buffer: gst::Buffer,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut state = self.state.lock().unwrap();

        if state.direct_eos {
            gst::debug!(CAT, obj = pad, "Dropping buffer after direct EOS");
            return Err(gst::FlowError::Eos);
        }
        if state.flushing {
            return Err(gst::FlowError::Flushing);
        }
        if state.buffer.is_some() {
            gst::debug!(CAT, obj = pad, "Already have the still frame");
            return Err(gst::FlowError::Eos);
        }

        let Some(current_caps) = state.current_caps.clone() else {
            gst::error!(CAT, obj = pad, "Received a buffer before caps");
            return Err(gst::FlowError::NotNegotiated);
        };

        state.buffer = Some(buffer);
        state.buffer_caps = Some(current_caps);
        self.srcpad.mark_reconfigure();
        drop(state);

        self.start_src_task().map_err(|err| {
            gst::error!(CAT, imp = self, "Failed to start source task: {err}");
            gst::FlowError::Error
        })?;

        // Refuse further input. Image decoders commonly use this to stop after
        // producing their first frame.
        Err(gst::FlowError::Eos)
    }

    fn sink_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
        gst::log!(CAT, obj = pad, "Handling sink event {event:?}");

        match event.view() {
            gst::EventView::Caps(caps) => {
                self.state.lock().unwrap().current_caps = Some(caps.caps_owned());
                true
            }
            gst::EventView::Eos(_) => {
                let mut state = self.state.lock().unwrap();
                if state.buffer.is_none() {
                    drop(state);
                    self.srcpad.push_event(event)
                } else {
                    state.seqnum = Some(event.seqnum());
                    true
                }
            }
            gst::EventView::Segment(_) => {
                self.state.lock().unwrap().seqnum = Some(event.seqnum());
                true
            }
            gst::EventView::FlushStart(_) => {
                self.state.lock().unwrap().reset_runtime();
                self.srcpad.push_event(event)
            }
            gst::EventView::FlushStop(_) => {
                self.state.lock().unwrap().flushing = false;
                self.srcpad.push_event(event)
            }
            _ => {
                let sticky = event.is_sticky();
                let result = self.srcpad.push_event(event);
                result || sticky
            }
        }
    }

    fn seek_start_stop(
        &self,
        start: GenericFormattedValue,
        stop: GenericFormattedValue,
    ) -> Option<(Option<gst::ClockTime>, Option<gst::ClockTime>)> {
        if start.format() == gst::Format::Time {
            Some((start.try_into().ok()?, stop.try_into().ok()?))
        } else if start.format() == gst::Format::Default {
            let start = self.convert_value(start, gst::Format::Time)?;
            let stop = self.convert_value(stop, gst::Format::Time)?;
            Some((start.try_into().ok()?, stop.try_into().ok()?))
        } else {
            gst::error!(CAT, imp = self, "Unsupported seek format");
            None
        }
    }

    fn perform_seek(&self, event: &gst::event::Seek) -> bool {
        let (rate, flags, start_type, start, stop_type, stop) = event.get();
        if rate == 0.0 {
            return false;
        }

        let Some((start, stop)) = self.seek_start_stop(start, stop) else {
            return false;
        };

        let seqnum = event.seqnum();
        let flushing = flags.contains(gst::SeekFlags::FLUSH);

        if flushing {
            self.state.lock().unwrap().flushing = true;
            self.srcpad
                .push_event(gst::event::FlushStart::builder().seqnum(seqnum).build());
        } else if let Err(err) = self.srcpad.pause_task() {
            gst::warning!(CAT, imp = self, "Failed to pause task for seek: {err}");
        }

        let stream_lock = self.srcpad.stream_lock();
        let (start_task, position, segment_seek) = {
            let mut state = self.state.lock().unwrap();
            let segment_seek = state
                .segment
                .do_seek(rate, flags, start_type, start, stop_type, stop)
                .is_some();

            if let Some(duration) = state.duration {
                let stop = state
                    .segment
                    .stop()
                    .map_or(duration, |stop| stop.min(duration));
                state.segment.set_stop(Some(stop));
                state.segment.set_duration(Some(duration));
            }

            state.need_segment = true;
            state.seqnum = Some(seqnum);
            state.flushing = false;
            (
                state.buffer.is_some(),
                state.segment.position(),
                segment_seek,
            )
        };

        if flushing {
            self.srcpad
                .push_event(gst::event::FlushStop::builder(true).seqnum(seqnum).build());
        }

        drop(stream_lock);

        if !segment_seek {
            return false;
        }

        if flags.contains(gst::SeekFlags::SEGMENT) {
            let _ = self.obj().post_message(
                gst::message::SegmentStart::builder(position)
                    .src(&*self.obj())
                    .build(),
            );
        }

        if start_task && let Err(err) = self.start_src_task() {
            gst::error!(CAT, imp = self, "Failed to restart task after seek: {err}");
            return false;
        }

        true
    }

    fn src_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
        gst::log!(CAT, obj = pad, "Handling source event {event:?}");

        match event.view() {
            gst::EventView::Navigation(_)
            | gst::EventView::Qos(_)
            | gst::EventView::Latency(_)
            | gst::EventView::Step(_) => true,
            gst::EventView::Seek(seek) => self.perform_seek(seek),
            gst::EventView::FlushStart(_) => {
                self.state.lock().unwrap().flushing = true;
                self.sinkpad.push_event(event)
            }
            gst::EventView::FlushStop(_) => {
                let mut state = self.state.lock().unwrap();
                state.reset_runtime();
                state.flushing = false;
                drop(state);
                self.sinkpad.push_event(event)
            }
            _ => self.sinkpad.push_event(event),
        }
    }

    fn sink_query(&self, pad: &gst::Pad, query: &mut gst::QueryRef) -> bool {
        match query.view_mut() {
            gst::QueryViewMut::Caps(query) => {
                let caps = self.query_caps(pad, query.filter());
                query.set_result(&caps);
                true
            }
            _ => gst::Pad::query_default(pad, Some(&*self.obj()), query),
        }
    }

    fn src_query(&self, pad: &gst::Pad, query: &mut gst::QueryRef) -> bool {
        match query.view_mut() {
            gst::QueryViewMut::Convert(query) => {
                let (source, destination_format) = query.get();
                let Some(destination) = self.convert_value(source, destination_format) else {
                    return false;
                };
                query.set(source, destination);
                true
            }
            gst::QueryViewMut::Position(query) => {
                let state = self.state.lock().unwrap();
                match query.format() {
                    gst::Format::Time => {
                        query.set(state.segment.position());
                        true
                    }
                    gst::Format::Default => {
                        query.set(gst::format::Default::from_u64(state.offset));
                        true
                    }
                    _ => false,
                }
            }
            gst::QueryViewMut::Duration(query) => {
                let state = self.state.lock().unwrap();
                match query.format() {
                    gst::Format::Time => {
                        query.set(state.duration);
                        true
                    }
                    gst::Format::Default => {
                        query.set(state.duration.map(|duration| {
                            gst::format::Default::from_u64(Self::frames_for_time(
                                duration,
                                state.framerate,
                            ))
                        }));
                        true
                    }
                    _ => false,
                }
            }
            gst::QueryViewMut::Seeking(query) => match query.format() {
                gst::Format::Time => {
                    let duration = self.state.lock().unwrap().duration;
                    query.set(true, gst::ClockTime::ZERO, duration);
                    true
                }
                gst::Format::Default => {
                    let state = self.state.lock().unwrap();
                    let duration = state.duration.map(|duration| {
                        gst::format::Default::from_u64(Self::frames_for_time(
                            duration,
                            state.framerate,
                        ))
                    });
                    query.set(true, gst::format::Default::ZERO, duration);
                    true
                }
                _ => false,
            },
            gst::QueryViewMut::Latency(query) => {
                query.set(false, gst::ClockTime::ZERO, gst::ClockTime::NONE);
                true
            }
            gst::QueryViewMut::Caps(query) => {
                let caps = self.query_caps(pad, query.filter());
                query.set_result(&caps);
                true
            }
            _ => gst::Pad::query_default(pad, Some(&*self.obj()), query),
        }
    }

    fn src_activatemode(
        &self,
        pad: &gst::Pad,
        mode: gst::PadMode,
        active: bool,
    ) -> Result<(), gst::LoggableError> {
        if mode != gst::PadMode::Push {
            return Err(gst::loggable_error!(CAT, "Only push mode is supported"));
        }

        if active {
            let mut state = self.state.lock().unwrap();
            state.reset_runtime();
            state.flushing = false;
        } else {
            self.state.lock().unwrap().flushing = true;
            pad.stop_task()
                .map_err(|err| gst::loggable_error!(CAT, "Failed to stop source task: {err}"))?;
            self.state.lock().unwrap().reset_runtime();
        }
        Ok(())
    }

    fn src_loop(&self) {
        let result = self.src_loop_inner();
        if let Err(flow_error) = result {
            self.finish_src_task(flow_error);
        }
    }

    fn src_loop_inner(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut first = false;
        let mut state = self.state.lock().unwrap();

        if state.flushing {
            return Err(gst::FlowError::Flushing);
        }
        if state.direct_eos {
            drop(state);
            let _ = self.srcpad.pause_task();
            return Ok(gst::FlowSuccess::Ok);
        }

        let mut buffer = state.buffer.clone().ok_or(gst::FlowError::Error)?;

        if self.srcpad.check_reconfigure() {
            let input_caps = state
                .buffer_caps
                .clone()
                .ok_or(gst::FlowError::NotNegotiated)?;
            let output_caps = Self::output_caps(&input_caps, state.framerate);
            drop(state);

            if !self.srcpad.push_event(gst::event::Caps::new(&output_caps)) {
                self.srcpad.mark_reconfigure();
                return Err(gst::FlowError::NotNegotiated);
            }
            state = self.state.lock().unwrap();
        }

        if state.need_segment {
            let segment = state.segment.clone();
            let seqnum = state.seqnum;
            state.offset = if segment.rate() >= 0.0 {
                segment
                    .start()
                    .map(|start| Self::frames_for_time(start, state.framerate))
                    .unwrap_or(0)
            } else {
                segment
                    .stop()
                    .map(|stop| Self::frames_for_time(stop, state.framerate))
                    .unwrap_or(0)
            };
            state.need_segment = false;
            first = true;
            drop(state);

            let mut event = gst::event::Segment::new(&segment);
            if let Some(seqnum) = seqnum {
                event.make_mut().set_seqnum(seqnum);
            }
            self.srcpad.push_event(event);
            state = self.state.lock().unwrap();
        }

        let offset = state.offset;
        let framerate = state.framerate;
        let timestamp = Self::time_for_frame(offset, framerate);
        let timestamp_end = Self::time_for_frame(offset.saturating_add(1), framerate);

        let eos = (state.segment.rate() >= 0.0
            && state.segment.stop().is_some_and(|stop| timestamp > stop))
            || (state.segment.rate() < 0.0 && offset == 0)
            || (state.segment.rate() < 0.0
                && state
                    .segment
                    .start()
                    .is_some_and(|start| timestamp_end < start));

        let clipped = state.segment.clip(Some(timestamp), Some(timestamp_end));
        if let Some((clip_start, clip_stop)) = clipped {
            let clip_start = clip_start.expect("defined input timestamp became undefined");
            let clip_stop = clip_stop.expect("defined input timestamp became undefined");
            let position = if state.segment.rate() >= 0.0 {
                clip_stop
            } else {
                clip_start
            };
            state.segment.set_position(Some(position));
        }

        if state.segment.rate() >= 0.0 {
            state.offset = state.offset.saturating_add(1);
        } else {
            state.offset = state.offset.saturating_sub(1);
        }
        drop(state);

        if let Some((clip_start, clip_stop)) = clipped {
            let clip_start = clip_start.expect("defined input timestamp became undefined");
            let clip_stop = clip_stop.expect("defined input timestamp became undefined");
            let buffer_mut = buffer.make_mut();
            buffer_mut.set_dts(gst::ClockTime::NONE);
            buffer_mut.set_pts(Some(clip_start));
            buffer_mut.set_duration(Some(clip_stop - clip_start));
            buffer_mut.set_offset(offset);
            buffer_mut.set_offset_end(offset.saturating_add(1));
            if first {
                buffer_mut.set_flags(gst::BufferFlags::DISCONT);
            } else {
                buffer_mut.unset_flags(gst::BufferFlags::DISCONT);
            }

            self.srcpad.push(buffer)?;
        }

        if eos {
            Err(gst::FlowError::Eos)
        } else {
            Ok(gst::FlowSuccess::Ok)
        }
    }

    fn finish_src_task(&self, flow_error: gst::FlowError) {
        gst::log!(CAT, imp = self, "Pausing source task: {flow_error}");
        let _ = self.srcpad.pause_task();

        if flow_error == gst::FlowError::Flushing {
            return;
        }

        let state = self.state.lock().unwrap();
        let seqnum = state.seqnum;
        let segment = state.segment.clone();
        drop(state);

        if flow_error == gst::FlowError::Eos {
            if segment.flags().contains(gst::SegmentFlags::SEGMENT) {
                let position = if segment.rate() >= 0.0 {
                    segment.stop()
                } else {
                    segment.start()
                };
                let _ = self.obj().post_message(
                    gst::message::SegmentDone::builder(position)
                        .src(&*self.obj())
                        .build(),
                );
                self.srcpad
                    .push_event(gst::event::SegmentDone::new(position));
            } else {
                let mut event = gst::event::Eos::new();
                if let Some(seqnum) = seqnum {
                    event.make_mut().set_seqnum(seqnum);
                }
                self.srcpad.push_event(event);
            }
        } else {
            gst::element_imp_error!(
                self,
                gst::StreamError::Failed,
                ("Internal data flow error"),
                ["source task stopped with {flow_error}"]
            );
            self.srcpad.push_event(gst::event::Eos::new());
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for ImageFreeze {
    const NAME: &'static str = "NtscRsImageFreeze";
    type Type = super::elements::ImageFreeze;
    type ParentType = gst::Element;

    fn with_class(class: &Self::Class) -> Self {
        let sinkpad = gst::Pad::builder_from_template(&class.pad_template("sink").unwrap())
            .chain_function(|pad, parent, buffer| {
                Self::catch_panic_pad_function(
                    parent,
                    || Err(gst::FlowError::Error),
                    |image_freeze| image_freeze.sink_chain(pad, buffer),
                )
            })
            .event_function(|pad, parent, event| {
                Self::catch_panic_pad_function(
                    parent,
                    || false,
                    |image_freeze| image_freeze.sink_event(pad, event),
                )
            })
            .query_function(|pad, parent, query| {
                Self::catch_panic_pad_function(
                    parent,
                    || false,
                    |image_freeze| image_freeze.sink_query(pad, query),
                )
            })
            .flags(gst::PadFlags::PROXY_ALLOCATION)
            .build();

        let srcpad = gst::Pad::builder_from_template(&class.pad_template("src").unwrap())
            .event_function(|pad, parent, event| {
                Self::catch_panic_pad_function(
                    parent,
                    || false,
                    |image_freeze| image_freeze.src_event(pad, event),
                )
            })
            .query_function(|pad, parent, query| {
                Self::catch_panic_pad_function(
                    parent,
                    || false,
                    |image_freeze| image_freeze.src_query(pad, query),
                )
            })
            .activatemode_function(|pad, parent, mode, active| {
                Self::catch_panic_pad_function(
                    parent,
                    || Err(gst::loggable_error!(CAT, "source activation panicked")),
                    |image_freeze| image_freeze.src_activatemode(pad, mode, active),
                )
            })
            .build();
        Self {
            sinkpad,
            srcpad,
            state: Mutex::new(State::new()),
        }
    }
}

impl ObjectImpl for ImageFreeze {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                gst::ParamSpecFraction::builder("framerate")
                    .nick("Framerate")
                    .blurb("Framerate of the generated video stream")
                    .minimum(gst::Fraction::new(1, i32::MAX))
                    .maximum(gst::Fraction::new(i32::MAX, 1))
                    .default_value(default_framerate())
                    .mutable_playing()
                    .build(),
                glib::ParamSpecUInt64::builder("duration")
                    .nick("Duration")
                    .blurb("Duration in nanoseconds; GST_CLOCK_TIME_NONE means unlimited")
                    .maximum(u64::MAX)
                    .default_value(u64::MAX)
                    .mutable_ready()
                    .build(),
            ]
        });
        PROPERTIES.as_ref()
    }

    fn constructed(&self) {
        self.parent_constructed();
        self.obj().add_pad(&self.sinkpad).unwrap();
        self.obj().add_pad(&self.srcpad).unwrap();
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match pspec.name() {
            "framerate" => {
                let framerate = value.get::<gst::Fraction>().unwrap();
                let changed = {
                    let mut state = self.state.lock().unwrap();
                    let changed = state.framerate != framerate;
                    state.framerate = framerate;
                    changed
                };
                if changed {
                    self.srcpad.mark_reconfigure();
                }
            }
            "duration" => {
                let raw_duration = value.get::<u64>().unwrap();
                let duration =
                    (raw_duration != u64::MAX).then(|| gst::ClockTime::from_nseconds(raw_duration));
                let mut state = self.state.lock().unwrap();
                state.duration = duration;
                state.reset_segment();
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        let state = self.state.lock().unwrap();
        match pspec.name() {
            "framerate" => state.framerate.to_value(),
            "duration" => state
                .duration
                .map(gst::ClockTime::nseconds)
                .unwrap_or(u64::MAX)
                .to_value(),
            _ => unimplemented!(),
        }
    }
}

impl GstObjectImpl for ImageFreeze {}

impl ElementImpl for ImageFreeze {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "ntsc-rs still frame stream generator",
                "Filter/Video",
                "Generates a finite or unlimited raw video stream from one frame",
                "ntsc-rs contributors",
            )
        });
        Some(&*METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let caps = gst::Caps::builder_full_with_any_features()
                .structure(gst::Structure::builder("video/x-raw").build())
                .build();
            vec![
                gst::PadTemplate::new(
                    "src",
                    gst::PadDirection::Src,
                    gst::PadPresence::Always,
                    &caps,
                )
                .unwrap(),
                gst::PadTemplate::new(
                    "sink",
                    gst::PadDirection::Sink,
                    gst::PadPresence::Always,
                    &caps,
                )
                .unwrap(),
            ]
        });
        PAD_TEMPLATES.as_ref()
    }

    fn send_event(&self, event: gst::Event) -> bool {
        if matches!(event.view(), gst::EventView::Eos(_)) {
            let mut state = self.state.lock().unwrap();
            state.direct_eos = true;
            drop(state);
            let _ = self.srcpad.pause_task();
            self.srcpad.push_event(event)
        } else {
            self.parent_send_event(event)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type BufferTiming = (Option<gst::ClockTime>, Option<gst::ClockTime>, u64);

    fn run_pipeline(seek: Option<(f64, gst::ClockTime, gst::ClockTime)>) -> Vec<BufferTiming> {
        gst::init().unwrap();

        let pipeline = gst::Pipeline::default();
        let source = gst::ElementFactory::make("videotestsrc")
            .property("num-buffers", 1i32)
            .build()
            .unwrap();
        let input_caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "RGBx")
                    .field("width", 16i32)
                    .field("height", 16i32)
                    .field("framerate", gst::Fraction::new(0, 1))
                    .build(),
            )
            .build()
            .unwrap();
        let freeze: super::super::elements::ImageFreeze = glib::Object::builder()
            .property("framerate", gst::Fraction::new(25, 1))
            .property("duration", gst::ClockTime::from_mseconds(400).nseconds())
            .build();
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
            .unwrap();

        pipeline
            .add_many([&source, &input_caps, freeze.upcast_ref(), &sink])
            .unwrap();
        gst::Element::link_many([&source, &input_caps, freeze.upcast_ref(), &sink]).unwrap();

        let buffers = Arc::new(Mutex::new(Vec::new()));
        let buffers_clone = Arc::clone(&buffers);
        sink.static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(gst::PadProbeData::Buffer(buffer)) = &info.data {
                    buffers_clone.lock().unwrap().push((
                        buffer.pts(),
                        buffer.duration(),
                        buffer.offset(),
                    ));
                }
                gst::PadProbeReturn::Ok
            });

        if let Some((rate, start, stop)) = seek {
            pipeline.set_state(gst::State::Paused).unwrap();
            freeze
                .seek(
                    rate,
                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                    gst::SeekType::Set,
                    start,
                    gst::SeekType::Set,
                    stop,
                )
                .unwrap();
        }

        pipeline.set_state(gst::State::Playing).unwrap();
        let message = pipeline.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        pipeline.set_state(gst::State::Null).unwrap();

        let message = message.expect("pipeline did not finish");
        if let gst::MessageView::Error(error) = message.view() {
            panic!("pipeline failed: {} ({:?})", error.error(), error.debug());
        }

        buffers.lock().unwrap().clone()
    }

    #[test]
    fn produces_a_finite_stream_with_expected_timestamps() {
        let buffers = run_pipeline(None);
        assert_eq!(buffers.len(), 10);
        for (index, &(pts, duration, offset)) in buffers.iter().enumerate() {
            assert_eq!(pts, Some(gst::ClockTime::from_mseconds(index as u64 * 40)));
            assert_eq!(duration, Some(gst::ClockTime::from_mseconds(40)));
            assert_eq!(offset, index as u64);
        }
    }

    #[test]
    fn clips_a_non_frame_aligned_seek() {
        let buffers = run_pipeline(Some((
            1.0,
            gst::ClockTime::from_mseconds(220),
            gst::ClockTime::from_mseconds(380),
        )));
        assert_eq!(buffers.len(), 5);
        assert_eq!(
            buffers[0],
            (
                Some(gst::ClockTime::from_mseconds(220)),
                Some(gst::ClockTime::from_mseconds(20)),
                5,
            )
        );
        assert_eq!(
            buffers[4],
            (
                Some(gst::ClockTime::from_mseconds(360)),
                Some(gst::ClockTime::from_mseconds(20)),
                9,
            )
        );
    }

    #[test]
    fn supports_reverse_seeks() {
        let buffers = run_pipeline(Some((
            -1.0,
            gst::ClockTime::ZERO,
            gst::ClockTime::from_mseconds(400),
        )));
        assert_eq!(buffers.len(), 10);
        assert_eq!(
            buffers.first().unwrap().0,
            Some(gst::ClockTime::from_mseconds(360))
        );
        assert_eq!(buffers.last().unwrap().0, Some(gst::ClockTime::ZERO));
    }
}
