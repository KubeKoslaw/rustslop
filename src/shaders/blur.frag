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
uniform float alpha;
uniform vec2 step_size;
varying vec2 v_coords;
void main() {
    vec4 c = texture2D(tex, v_coords) * 0.227027;
    c += texture2D(tex, v_coords + step_size * 1.384615) * 0.316216;
    c += texture2D(tex, v_coords - step_size * 1.384615) * 0.316216;
    c += texture2D(tex, v_coords + step_size * 3.230769) * 0.070270;
    c += texture2D(tex, v_coords - step_size * 3.230769) * 0.070270;
    gl_FragColor = c * alpha;
}
