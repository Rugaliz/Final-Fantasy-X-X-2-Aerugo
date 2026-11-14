#version 450
// Textured, optionally skinned mesh. Joint matrices come from a storage buffer; `params.x` is the first joint
// of this draw and `params.y` > 0.5 enables skinning.

layout(location = 0) in vec3 inPosition;
layout(location = 1) in vec3 inNormal;
layout(location = 2) in vec2 inUv;
layout(location = 3) in vec4 inColor;
layout(location = 4) in vec4 inWeights;
layout(location = 5) in uvec4 inJoints;

layout(push_constant) uniform Push {
    mat4 viewProj;
    vec4 model0;
    vec4 model1;
    vec4 model2;
    vec4 params;
} pc;

layout(set = 1, binding = 0) readonly buffer Joints {
    mat4 joints[];
};

layout(location = 0) out vec2 vUv;
layout(location = 1) out vec4 vColor;
layout(location = 2) out vec3 vNormal;

void main() {
    vec4 p = vec4(inPosition, 1.0);
    vec3 n = inNormal;
    if (pc.params.y > 0.5) {
        uint base = uint(pc.params.x);
        mat4 skin = inWeights.x * joints[base + inJoints.x]
                  + inWeights.y * joints[base + inJoints.y]
                  + inWeights.z * joints[base + inJoints.z]
                  + inWeights.w * joints[base + inJoints.w];
        p = skin * p;
        n = mat3(skin) * n;
    }
    // Model transform as three rows (affine).
    vec3 world = vec3(dot(pc.model0, p), dot(pc.model1, p), dot(pc.model2, p));
    vec3 wn = vec3(dot(pc.model0.xyz, n), dot(pc.model1.xyz, n), dot(pc.model2.xyz, n));
    gl_Position = pc.viewProj * vec4(world, 1.0);
    vUv = inUv;
    vColor = inColor;
    vNormal = wn;
}
