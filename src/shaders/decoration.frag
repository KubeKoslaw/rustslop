precision mediump float;
uniform float alpha;
uniform vec2 window_size;
uniform float padding;
uniform float radius;
uniform float border_width;
uniform vec4 border_color;
uniform float shadow_size;
uniform float shadow_opacity;
uniform float shadow_offset;
varying vec2 v_coords;
float rounded(vec2 p, float r) {
    vec2 q = abs(p) - window_size * 0.5 + vec2(r);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
    vec2 p = (v_coords - 0.5) * (window_size + 2.0 * padding);
    float r = min(radius, min(window_size.x, window_size.y) * 0.5);
    float d = rounded(p, r);
    float ring = smoothstep(-0.5, 0.5, d) * (1.0 - smoothstep(border_width-0.5, border_width+0.5, d));
    float sd = max(rounded(p - vec2(0.0, shadow_offset), r), 0.0);
    float shadow = exp(-3.0 * sd * sd / max(shadow_size * shadow_size, 1.0)) * shadow_opacity;
    shadow *= smoothstep(-0.5, 0.5, d) * (1.0 - smoothstep(padding-2.0, padding, sd));
    float a = ring * border_color.a;
    gl_FragColor = vec4(border_color.rgb * a, a + shadow * (1.0-a)) * alpha;
}
