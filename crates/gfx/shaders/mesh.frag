#version 450
// Base texture × vertex colour, alpha test, simple two-sided lambert lighting (viewer), the remaster's field and
// sky colour curves and its water and ice shaders.

layout(location = 0) in vec2 vUv;
layout(location = 1) in vec4 vColor;
layout(location = 2) in vec3 vNormal;
layout(location = 3) in float vDepth;
layout(location = 4) in vec4 vUvA;
layout(location = 5) in vec2 vUvB;
layout(location = 6) in vec3 vWorld;
layout(location = 7) in vec3 vTangent;

layout(set = 0, binding = 0) uniform texture2D baseTexture;
layout(set = 0, binding = 1) uniform sampler baseSampler;

// Slot 0: scene constants (fog colour, enabled; near, far, curve, limit; eye position, time); joint matrices
// follow, then two slots per water part.
layout(set = 1, binding = 0) readonly buffer Joints {
    mat4 joints[];
};

// Water: second base texture, two normal maps, cube map. Ice: specular map, (unused), (unused), cube map.
layout(set = 2, binding = 0) uniform texture2D secondTexture;
layout(set = 2, binding = 1) uniform texture2D normalMap1;
layout(set = 2, binding = 2) uniform texture2D normalMap2;
layout(set = 2, binding = 3) uniform textureCube cubeMap;

layout(push_constant) uniform Push {
    mat4 viewProj;
    vec4 model0;
    vec4 model1;
    vec4 model2;
    vec4 params;
} pc;

layout(location = 0) out vec4 outColor;
// World normal and a marker for the ambient occlusion: 1 scenery (field shading), 0.5 characters and the rest.
layout(location = 1) out vec4 outNormal;

// The fog share at this fragment's view depth (0 when the scene has no fog or the part ignores it).
float fogShare() {
    vec4 fogColor = joints[0][0];
    vec4 fog = joints[0][1];
    if (fogColor.w < 0.5 || pc.params.w > 7.5) {
        return 0.0;
    }
    float f = clamp((abs(vDepth) - fog.x) / (fog.y - fog.x + 0.01), 0.0, 1.0);
    return min(pow(f, fog.z), fog.w);
}

// A game-space direction in the map's own space, where the cube maps are oriented (maps are placed with
// x and y negated).
vec3 mapDir(vec3 d) {
    return vec3(-d.x, -d.y, d.z);
}

vec4 water() {
    mat4 w0 = joints[uint(pc.params.x)];
    mat4 w1 = joints[uint(pc.params.x) + 1u];
    vec3 n0 = normalize(vNormal);
    vec3 t = normalize(vTangent);
    vec3 nm = texture(sampler2D(normalMap1, baseSampler), vUvA.zw).xyz
            + texture(sampler2D(normalMap2, baseSampler), vUvB).xyz - 1.0;
    nm = normalize(nm);
    vec3 n = nm.x * t + nm.y * cross(t, n0) + nm.z * n0;
    vec3 v = normalize(vWorld - joints[0][2].xyz);
    vec3 r = v - 2.0 * dot(v, n) * n;
    vec4 env = texture(samplerCube(cubeMap, baseSampler), mapDir(n));
    vec4 refl = texture(samplerCube(cubeMap, baseSampler), mapDir(r)) * w0[2].w;
    refl.rgb *= w0[2].rgb;
    vec4 base = texture(sampler2D(baseTexture, baseSampler), vUv) + vec4(0.0, 0.0, 0.0, 1.0)
              + texture(sampler2D(secondTexture, baseSampler), vUvA.xy);
    vec4 c = base * w0[3] * env + refl;
    vec3 toFog = joints[0][0].rgb - c.rgb * vColor.rgb;
    c *= vColor;
    c.rgb += w1[1].rgb * toFog * fogShare();
    return clamp(c * w1[0], 0.0, 1.0);
}

// The remaster's ice shader: texture × colour × cube(n) + cube(reflect(view, n)) × shininess × specular map,
// then × vertex colour, fog, colour scale (the sky curve follows).
vec4 ice() {
    mat4 w0 = joints[uint(pc.params.x)];
    mat4 w1 = joints[uint(pc.params.x) + 1u];
    vec3 n = normalize(vNormal);
    vec3 v = normalize(vWorld - joints[0][2].xyz);
    vec3 r = v - 2.0 * dot(v, n) * n;
    vec4 env = texture(samplerCube(cubeMap, baseSampler), mapDir(n));
    vec4 refl = texture(samplerCube(cubeMap, baseSampler), mapDir(r)) * w0[2].w
              * texture(sampler2D(secondTexture, baseSampler), vUv);
    vec4 c = texture(sampler2D(baseTexture, baseSampler), vUv) * w0[3] * env + refl;
    vec3 toFog = joints[0][0].rgb - c.rgb * vColor.rgb;
    c *= vColor;
    c.rgb += w1[1].rgb * toFog * fogShare();
    return c * w1[0];
}

// params.w: 0 colours as they are, 1 simple lighting (viewer), 2 the remaster's field curve, 3 its sky curve,
// 4 its water shader (then the field curve), 5 its ice shader (then the sky curve), 6 map shading without a curve
// (clamped to 1).
// Codes over 7.5 are the same shading without fog.
void main() {
    float mode = pc.params.w > 7.5 ? pc.params.w - 8.0 : pc.params.w;
    vec4 c;
    if (mode > 4.5 && mode < 5.5) {
        c = ice();
    } else if (mode > 3.5 && mode < 4.5) {
        c = water();
    } else {
        vec4 t = texture(sampler2D(baseTexture, baseSampler), vUv);
        c = t * vColor;
        // The field shaders test the texture's own alpha; characters and the rest the coloured one.
        if ((mode > 1.5 ? t.a : c.a) < pc.params.z) {
            discard;
        }
    }
    vec3 rgb = c.rgb;
    if (mode > 0.5 && mode < 1.5) {
        vec3 n = normalize(vNormal);
        float d = abs(dot(n, normalize(vec3(0.4, 0.8, 0.45))));
        rgb *= 0.45 + 0.65 * d;
    }
    if ((mode > 1.5 && mode < 3.5) || mode > 5.5) {
        rgb += (joints[0][0].rgb - rgb) * fogShare();
    }
    // Characters (no tone curve of their own) fade toward the fog colour as the field curve shows it.
    if (mode < 0.5) {
        vec3 f = joints[0][0].rgb;
        vec3 fog = min(f, vec3(0.4)) * 2.1 + clamp(f, vec3(0.4), vec3(0.58)) * 1.08 - 0.47;
        rgb += (fog - rgb) * fogShare();
    }
    if ((mode > 1.5 && mode < 2.5) || (mode > 3.5 && mode < 4.5)) {
        rgb = min(rgb, vec3(0.4)) * 2.1 + clamp(rgb, vec3(0.4), vec3(0.58)) * 1.08 - 0.47;
    } else if (mode > 5.5) {
        rgb = min(rgb, vec3(1.0));
    } else if (mode > 2.5) {
        vec3 s = clamp(rgb, 0.0, 1.0);
        s = min(s * 0.3 + pow(s, vec3(0.588235)), vec3(1.0));
        rgb = 0.5 - 0.5 * cos(s * 3.14159);
    }
    outColor = vec4(rgb, c.a);
    float len = length(vNormal);
    outNormal = vec4(len > 0.0 ? vNormal / len : vec3(0.0), mode > 1.5 ? 1.0 : 0.5);
}
