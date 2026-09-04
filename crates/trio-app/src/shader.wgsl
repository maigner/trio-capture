// Composites three camera textures into slots, with zoom/pan and per-camera
// grading. Used unchanged for the live preview and for export.

struct Uniforms {
    out_size: vec4<f32>,             // width, height, unused, unused
    slot_rect: array<vec4<f32>, 3>,  // x, y, w, h (normalized, top-left origin)
    slot_params: array<vec4<f32>, 3>,// zoom, pan_x, pan_y, camera index
    src_info: array<vec4<f32>, 3>,   // per camera: width, height, has_frame, unused
    grade_a: array<vec4<f32>, 3>,    // exposure, contrast, saturation, temperature
    grade_b: array<vec4<f32>, 3>,    // tint, lift, gamma, gain
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var cam0: texture_2d<f32>;
@group(0) @binding(3) var cam1: texture_2d<f32>;
@group(0) @binding(4) var cam2: texture_2d<f32>;

struct VSOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VSOut {
    var out: VSOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

fn sample_cam(cam: i32, uv: vec2<f32>) -> vec3<f32> {
    if (cam == 0) { return textureSampleLevel(cam0, samp, uv, 0.0).rgb; }
    if (cam == 1) { return textureSampleLevel(cam1, samp, uv, 0.0).rgb; }
    return textureSampleLevel(cam2, samp, uv, 0.0).rgb;
}

fn lin_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_lin(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn grade(color: vec3<f32>, cam: i32) -> vec3<f32> {
    let a = u.grade_a[cam];
    let b = u.grade_b[cam];
    // Scene-linear operations.
    var c = color * exp2(a.x);
    c = c * vec3<f32>(1.0 + 0.25 * a.w, 1.0 - 0.25 * b.x, 1.0 - 0.25 * a.w);
    // Display-referred operations, like a classic grading panel.
    var d = lin_to_srgb(c);
    d = pow(max(d * b.w + b.y, vec3<f32>(0.0)), vec3<f32>(1.0 / max(b.z, 0.05)));
    d = (d - 0.5) * a.y + 0.5;
    let luma = dot(d, vec3<f32>(0.2126, 0.7152, 0.0722));
    d = mix(vec3<f32>(luma), d, a.z);
    return srgb_to_lin(clamp(d, vec3<f32>(0.0), vec3<f32>(1.0)));
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    // Later slots are overlays and win the hit test.
    for (var s: i32 = 2; s >= 0; s = s - 1) {
        let r = u.slot_rect[s];
        if (uv.x >= r.x && uv.y >= r.y && uv.x < r.x + r.z && uv.y < r.y + r.w) {
            let local = (uv - r.xy) / r.zw;
            let p = u.slot_params[s];
            let cam = i32(p.w + 0.5);
            let info = u.src_info[cam];
            if (info.z < 0.5) {
                return vec4<f32>(0.04, 0.04, 0.04, 1.0);
            }
            let slot_px = r.zw * u.out_size.xy;
            let slot_aspect = slot_px.x / max(slot_px.y, 1.0);
            let src_aspect = info.x / max(info.y, 1.0);
            var region = vec2<f32>(1.0, 1.0);
            if (src_aspect > slot_aspect) {
                region.x = slot_aspect / src_aspect;
            } else {
                region.y = src_aspect / slot_aspect;
            }
            region = region / max(p.x, 0.01);
            let center = clamp(vec2<f32>(0.5, 0.5) + p.yz, region * 0.5, vec2<f32>(1.0, 1.0) - region * 0.5);
            let suv = center - region * 0.5 + local * region;
            let c = sample_cam(cam, suv);
            return vec4<f32>(grade(c, cam), 1.0);
        }
    }
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
