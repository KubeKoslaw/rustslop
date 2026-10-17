#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform sampler2D backdrop;
uniform float alpha;
uniform vec2 extent;
uniform vec2 output_size;
uniform float radius;
uniform float use_blur;
varying vec2 v_coords;
float rounded(vec2 p, vec2 halfsize, float r) {
    vec2 q = abs(p) - halfsize + vec2(r);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
    vec4 c = texture2D(tex, v_coords);
    float r = min(radius, min(extent.x, extent.y) * 0.5);
    float d = rounded((v_coords - 0.5) * extent, extent * 0.5, r);
    float coverage = 1.0 - smoothstep(-0.5, 0.5, d);
    // Zero-alpha holes stay transparent, including the space between Waybar islands.
    float glass = use_blur * smoothstep(0.0, 0.2, c.a) * (1.0 - c.a);
    vec3 bg = texture2D(backdrop, gl_FragCoord.xy / output_size).rgb;
    gl_FragColor = vec4(c.rgb + bg * glass, c.a + glass) * coverage * alpha;
}
