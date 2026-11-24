//! The games' script machine. Scripts drive maps, cutscenes and battles: each worker (an entity) runs bytecode at
//! one of several priority levels, workers ask each other to run entries (requests), and natives connect scripts
//! to the engine. Game logic plugs in through [`Host`] (usually [`natives::Natives`] bound to a game state).

pub mod asm;
pub mod memory;
pub mod natives;
pub mod recording;
mod rsin;
pub mod vm;

pub use memory::Memory;
pub use natives::{Coverage, Native, Natives};
pub use vm::{
    Context, Fault, Host, NativeCall, Phase, Request, RequestMode, Stack, TraceEvent, Value, Vm, Wait, Worker,
};
