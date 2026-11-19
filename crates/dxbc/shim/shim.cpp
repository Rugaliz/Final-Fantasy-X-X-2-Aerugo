// C interface over dxbc-spirv for Aerugo: translates a Direct3D 11 shader (DXBC) to SPIR-V for Vulkan.
//
// The steps and options follow DXVK's use of the same library (src/d3d11/d3d11_shader.cpp and
// src/dxvk/dxvk_shader_ir.cpp in https://github.com/doitsujin/dxvk): convert, lower the sample-count built-in
// to a specialization constant, legalize, fix pixel shader inputs up against the vertex shader's outputs, and
// build SPIR-V with Aerugo's descriptor layout (see crates/dxbc/src/lib.rs).

#include <cstdlib>
#include <cstring>
#include <string>
#include <utility>

#include "dxbc/dxbc_container.h"
#include "dxbc/dxbc_converter.h"
#include "dxbc/dxbc_parser.h"
#include "ir/ir_builder.h"
#include "ir/ir_legalize.h"
#include "ir/passes/ir_pass_lower_io.h"
#include "spirv/spirv_builder.h"
#include "spirv/spirv_mapping.h"
#include "util/util_log.h"
#include "util/util_small_vector.h"

using namespace dxbc_spv;

namespace {

// Descriptor layout shared with lib.rs.
constexpr uint32_t BINDING_SAMPLER = 16u;
constexpr uint32_t BINDING_SRV = 32u;
constexpr uint32_t BINDING_UAV = 160u;

uint32_t stage_set(ir::ShaderStage stage) {
  switch (stage) {
    case ir::ShaderStage::ePixel: return 1u;
    case ir::ShaderStage::eGeometry: return 2u;
    case ir::ShaderStage::eHull: return 3u;
    case ir::ShaderStage::eDomain: return 4u;
    default: return 0u;  // vertex, compute
  }
}

class Mapping : public spirv::ResourceMapping {
public:
  explicit Mapping(ir::ShaderStage stage) : m_set(stage_set(stage)) {}

  spirv::DescriptorBinding mapDescriptor(ir::ScalarType type, uint32_t, uint32_t regIndex) override {
    spirv::DescriptorBinding b;
    b.set = m_set;
    switch (type) {
      case ir::ScalarType::eSampler: b.binding = BINDING_SAMPLER + regIndex; break;
      case ir::ScalarType::eSrv: b.binding = BINDING_SRV + regIndex; break;
      case ir::ScalarType::eUav:
      case ir::ScalarType::eUavCounter: b.binding = BINDING_UAV + regIndex; break;
      default: b.binding = regIndex; break;  // constant buffers
    }
    return b;
  }

  uint32_t mapPushData(ir::ShaderStageMask) override { return 0u; }

private:
  uint32_t m_set;
};

// Collects the library's messages so a failure can be reported to Rust.
class Log : public util::Logger {
public:
  void message(util::LogLevel severity, const char* text) override {
    if (severity >= util::LogLevel::eWarn) {
      if (!m_text.empty()) m_text += '\n';
      m_text += text;
    }
  }
  util::LogLevel getMinimumSeverity() override { return util::LogLevel::eWarn; }
  std::string m_text;
};

std::pair<ir::SsaDef, ir::ShaderStage> entry_point(ir::Builder& builder) {
  auto [a, b] = builder.getDeclarations();
  for (auto it = a; it != b; it++) {
    if (it->getOpCode() == ir::OpCode::eEntryPoint)
      return {it->getDef(), ir::ShaderStage(it->getOperand(it->getFirstLiteralOperandIndex()))};
  }
  return {ir::SsaDef(), ir::ShaderStage::eFlagEnum};
}

// As DXVK: a pixel shader's sample count (sample_info on the rasterizer) becomes specialization constant 0.
void lower_built_ins(ir::Builder& builder) {
  auto [ep, stage] = entry_point(builder);
  if (stage != ir::ShaderStage::ePixel) return;
  auto spec = builder.add(ir::Op::DclSpecConstant(ir::ScalarType::eU32, ep, 0u, 0u));
  util::small_vector<ir::SsaDef, 32u> inputs;
  for (auto it = builder.getDeclarations().first; it != builder.getDeclarations().second; it++) {
    if (it->getOpCode() == ir::OpCode::eDclInputBuiltIn) inputs.push_back(it->getDef());
  }
  for (auto def : inputs) {
    const auto& op = builder.getOp(def);
    if (ir::BuiltIn(op.getOperand(op.getFirstLiteralOperandIndex())) != ir::BuiltIn::eSampleCount) continue;
    util::small_vector<ir::SsaDef, 32u> uses;
    builder.getUses(def, uses);
    for (auto use : uses) {
      if (builder.getOp(use).getOpCode() == ir::OpCode::eInputLoad) builder.rewriteDef(use, spec);
    }
  }
}

bool convert(const void* data, size_t size, ir::Builder& builder) {
  dxbc::Converter::Options options = {};
  options.name = "main";
  options.includeDebugNames = true;
  options.boundCheckShaderIo = true;
  options.lowerIcb = false;
  options.icbRegisterSpace = 0u;
  options.icbRegisterIndex = 14u;  // D3D11_COMMONSHADER_CONSTANT_BUFFER_API_SLOT_COUNT
  options.classInstanceRegisterSpace = 0u;
  options.classInstanceRegisterIndex = 15u;
  options.limitTessFactor = true;
  dxbc::Container container(data, size);
  dxbc::Converter converter(std::move(container), options);
  if (!converter.convertShader(builder)) return false;
  lower_built_ins(builder);
  return true;
}

void legalize(ir::Builder& builder) {
  // DXVK's options for a device with 16-byte storage-buffer alignment and no optional lowering workarounds.
  ir::CompileOptions o;
  o.arithmeticOptions.lowerDot = true;
  o.arithmeticOptions.lowerSinCos = false;
  o.arithmeticOptions.lowerMsad = true;
  o.arithmeticOptions.lowerUdiv = true;
  o.arithmeticOptions.lowerF32toF16 = false;
  o.arithmeticOptions.lowerConvertFtoI = false;
  o.arithmeticOptions.lowerGsVertexCountIn = false;
  o.arithmeticOptions.hasNvUnsignedItoFBug = false;
  o.min16Options.enableFloat16 = false;
  o.min16Options.enableInt16 = false;
  o.resourceOptions.allowSubDwordScratchAndLds = true;
  o.resourceOptions.flattenLds = false;
  o.resourceOptions.flattenScratch = false;
  o.resourceOptions.structuredCbv = true;
  o.resourceOptions.structuredSrvUav = true;
  const uint32_t ssbo_alignment = 16u;
  o.bufferOptions.useTypedForRaw = ssbo_alignment > 16u;
  o.bufferOptions.useTypedForStructured = ssbo_alignment > 4u;
  o.bufferOptions.useTypedForSparseFeedback = true;
  o.bufferOptions.useRawForTypedAtomic = ssbo_alignment <= 4u;
  o.bufferOptions.forceFormatForTypedUavRead = false;
  o.bufferOptions.minStructureAlignment = ssbo_alignment;
  o.scalarizeOptions.subDwordVectors = true;
  o.syncOptions.insertRovLocks = true;
  o.syncOptions.insertLdsBarriers = false;
  o.syncOptions.insertUavBarriers = false;
  o.derivativeOptions.hoistNontrivialDerivativeOps = true;
  o.derivativeOptions.hoistNontrivialImplicitLodOps = false;
  o.derivativeOptions.hoistDescriptorLoads = true;
  o.cseOptions.relocateDescriptorLoad = true;
  o.cseOptions.resolveOverlappingLoads = true;
  o.bufferIndexing.allowDynamicIndexing = true;
  ir::legalizeIr(builder, o);
}

char* copy_text(const std::string& s) {
  char* out = static_cast<char*>(std::malloc(s.size() + 1));
  if (out) std::memcpy(out, s.c_str(), s.size() + 1);
  return out;
}

}  // namespace

extern "C" {

// Translates `code` (a DXBC container). For a pixel shader, `prev` may hold the vertex shader it is drawn with,
// whose outputs its inputs are matched against. On success returns 0 and sets `*words`/`*count` (free with
// aerugo_dxbc_free); on failure returns 1 and sets `*error` to a message (free with aerugo_dxbc_free_text).
// `float_controls`: bit 0 independent denorm modes, 1 RTE32, 2 RTZ32, 3 denorm flush 32, 4 denorm preserve 32,
// 5 signed zero / inf / nan preserve 32 (the device's VkPhysicalDeviceFloatControlsProperties).
int aerugo_dxbc_to_spirv(const uint8_t* code, size_t size, const uint8_t* prev, size_t prev_size,
                         uint32_t float_controls, uint32_t** words, size_t* count, char** error) {
  *words = nullptr;
  *count = 0;
  *error = nullptr;
  Log log;
  ir::Builder builder;
  if (!convert(code, size, builder)) {
    *error = copy_text(log.m_text.empty() ? "conversion failed" : log.m_text);
    return 1;
  }
  legalize(builder);
  auto stage = entry_point(builder).second;
  {
    ir::LowerIoPass io(builder);
    if (stage == ir::ShaderStage::ePixel) {
      io.resolveUnwrittenPsOutputs();
      if (prev && prev_size) {
        ir::Builder vs;
        if (!convert(prev, prev_size, vs)) {
          *error = copy_text("conversion of the vertex shader failed");
          return 1;
        }
        legalize(vs);
        auto outputs = ir::IoMap::forOutputs(vs, 0u);
        auto inputs = ir::IoMap::forInputs(builder);
        if (!ir::IoMap::checkCompatibility(ir::ShaderStage::eVertex, outputs, stage, inputs, false))
          io.resolveMismatchedIo(ir::ShaderStage::eVertex, outputs);
      }
    }
  }
  spirv::SpirvBuilder::Options options = {};
  options.includeDebugNames = true;
  options.floatControls2 = false;
  // As DXVK: only with independent denorm modes, RTE and denorm flush for 32-bit floats all supported.
  auto fc = [&](uint32_t bit) { return (float_controls & (1u << bit)) != 0u; };
  if (fc(0) && fc(1) && fc(3)) {
    if (fc(1)) options.supportedRoundModesF32 |= ir::RoundMode::eNearestEven;
    if (fc(2)) options.supportedRoundModesF32 |= ir::RoundMode::eZero;
    if (fc(3)) options.supportedDenormModesF32 |= ir::DenormMode::eFlush;
    if (fc(4)) options.supportedDenormModesF32 |= ir::DenormMode::ePreserve;
  }
  options.supportsZeroInfNanPreserveF32 = fc(5);
  Mapping mapping(stage);
  spirv::SpirvBuilder spv(builder, mapping, options);
  spv.buildSpirvBinary();
  size_t bytes = 0;
  spv.getSpirvBinary(bytes, nullptr);
  auto* out = static_cast<uint32_t*>(std::malloc(bytes));
  if (!out) {
    *error = copy_text("out of memory");
    return 1;
  }
  spv.getSpirvBinary(bytes, out);
  *words = out;
  *count = bytes / 4;
  return 0;
}

void aerugo_dxbc_free(uint32_t* words) { std::free(words); }

void aerugo_dxbc_free_text(char* text) { std::free(text); }
}
