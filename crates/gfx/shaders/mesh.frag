#version 450
// Base texture × vertex colour, alpha test, simple two-sided lambert lighting (viewer only; the games' own
// material model arrives with the effect system).

layout(location = 0) in vec2 vUv;
layout(location = 1) in vec4 vColor;
layout(location = 2) in vec3 vNormal;

layout(set = 0, binding = 0) uniform texture2D baseTexture;
layout(set = 0, binding = 1) uniform sampler baseSampler;

layout(push_constant) uniform Push {
    mat4 viewProj;
    vec4 model0;
    vec4 model1;
    vec4 model2;
    vec4 params;
} pc;

layout(location = 0) out vec4 outColor;

// params.w: 0 colours as they are, 1 simple lighting (viewer), 2 the remaster's field curve, 3 its sky curve.
void main() {
    vec4 c = texture(sampler2D(baseTexture, baseSampler), vUv) * vColor;
    if (c.a < pc.params.z) {
        discard;
    }
    float mode = pc.params.w;
    vec3 rgb = c.rgb;
    if (mode > 0.5 && mode < 1.5) {
        vec3 n = normalize(vNormal);
        float d = abs(dot(n, normalize(vec3(0.4, 0.8, 0.45))));
        rgb *= 0.45 + 0.65 * d;
    } else if (mode > 1.5 && mode < 2.5) {
        rgb = min(rgb, vec3(0.4)) * 2.1 + clamp(rgb, vec3(0.4), vec3(0.58)) * 1.08 - 0.47;
    } else if (mode > 2.5) {
        vec3 s = clamp(rgb, 0.0, 1.0);
        s = min(s * 0.3 + pow(s, vec3(0.588235)), vec3(1.0));
        rgb = 0.5 - 0.5 * cos(s * 3.14159);
    }
    outColor = vec4(rgb, c.a);
}
