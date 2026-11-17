#version 450
// Textured, optionally skinned mesh. Joint matrices come from a storage buffer after its first slot (scene
// constants); `params.x` is the first joint of this draw and `params.y` > 0.5 enables skinning.

layout(location = 0) in vec3 inPosition;
layout(location = 1) in vec3 inNormal;
layout(location = 2) in vec2 inUv;
layout(location = 3) in vec4 inColor;
layout(location = 4) in vec4 inWeights;
layout(location = 5) in uvec4 inJoints;
layout(location = 6) in vec2 inUv2;
layout(location = 7) in vec3 inTangent;

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
layout(location = 3) out float vDepth;
// Water only: second base texture and first normal map coordinates, second normal map coordinates, world
// position, world tangent.
layout(location = 4) out vec4 vUvA;
layout(location = 5) out vec2 vUvB;
layout(location = 6) out vec3 vWorld;
layout(location = 7) out vec3 vTangent;

vec3 rows(vec3 v) {
    return vec3(dot(pc.model0.xyz, v), dot(pc.model1.xyz, v), dot(pc.model2.xyz, v));
}

void main() {
    vec4 p = vec4(inPosition, 1.0);
    vec3 n = inNormal;
    // params.w >= 1.5: a field part; params.xy is then its texture scroll instead of joints. 4: water, with
    // params.x its first constant slot.
    bool field = pc.params.w > 1.5;
    bool water = pc.params.w > 3.5;
    if (!field && pc.params.y > 0.5) {
        uint base = uint(pc.params.x) + 1u;
        mat4 skin = inWeights.x * joints[base + inJoints.x]
                  + inWeights.y * joints[base + inJoints.y]
                  + inWeights.z * joints[base + inJoints.z]
                  + inWeights.w * joints[base + inJoints.w];
        p = skin * p;
        n = mat3(skin) * n;
    }
    // Model transform as three rows (affine).
    vec3 world = vec3(dot(pc.model0, p), dot(pc.model1, p), dot(pc.model2, p));
    gl_Position = pc.viewProj * vec4(world, 1.0);
    vColor = inColor;
    vNormal = rows(n);
    vWorld = world;
    vTangent = rows(inTangent);
    vUvA = vec4(0.0);
    vUvB = vec2(0.0);
    if (water) {
        // Four scrolling coordinates: base on the first set, second texture on the second, each normal map on
        // the set its constants say.
        mat4 w = joints[uint(pc.params.x)];
        vec4 sets = joints[uint(pc.params.x) + 1u][2];
        float t = joints[0][2].w;
        vUv = inUv + fract(w[0].xy * t);
        vec2 n1 = sets.x > 0.5 ? inUv2 : inUv;
        vec2 n2 = sets.y > 0.5 ? inUv2 : inUv;
        vUvA = vec4(inUv2 + fract(w[0].zw * t), n1 + fract(w[1].xy * t));
        vUvB = n2 + fract(w[1].zw * t);
    } else {
        vUv = field ? inUv + pc.params.xy : inUv;
    }
    // View depth (a perspective projection's w) for the fog.
    vDepth = gl_Position.w;
}
