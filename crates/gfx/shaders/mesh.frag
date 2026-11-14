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

void main() {
    vec4 c = texture(sampler2D(baseTexture, baseSampler), vUv) * vColor;
    if (c.a < pc.params.z) {
        discard;
    }
    float light = 1.0;
    if (pc.params.w > 0.5) {
        vec3 n = normalize(vNormal);
        float d = abs(dot(n, normalize(vec3(0.4, 0.8, 0.45))));
        light = 0.45 + 0.65 * d;
    }
    outColor = vec4(c.rgb * light, c.a);
}
