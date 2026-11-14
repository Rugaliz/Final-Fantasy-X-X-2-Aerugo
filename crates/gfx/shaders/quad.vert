#version 450

// One rotated, scaled rectangle per draw. Coordinates are in a square virtual space [-1, 1]
// which `scale` maps into the viewport without distortion.
layout(push_constant) uniform Push {
    vec2 center;
    vec2 half_size;
    vec2 rotation; // cos, sin
    vec2 scale;
    vec4 color;
} pc;

layout(location = 0) out vec4 v_color;

void main() {
    vec2 corners[6] = vec2[6](
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
        vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0)
    );
    vec2 c = corners[gl_VertexIndex] * pc.half_size;
    vec2 r = vec2(c.x * pc.rotation.x - c.y * pc.rotation.y, c.x * pc.rotation.y + c.y * pc.rotation.x);
    vec2 p = (pc.center + r) * pc.scale;
    gl_Position = vec4(p.x, -p.y, 0.0, 1.0);
    v_color = pc.color;
}
