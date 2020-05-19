//! This module contains enumeration of all legal Iroha Special Instructions `Instruction`,
//! generic instruction types and related implementations.
use crate::prelude::*;
use iroha_derive::Io;
use parity_scale_codec::{Decode, Encode};

pub mod prelude {
    //! Re-exports important traits and types. Meant to be glob imported when using `Iroha`.
    pub use crate::{account::isi::*, asset::isi::*, domain::isi::*, isi::*, peer::isi::*};
}

/// Enumeration of all legal Iroha Special Instructions.
#[derive(Clone, Debug, Io, Encode, Decode)]
pub enum Instruction {
    /// Variant of instructions related to `Peer`.
    Peer(crate::peer::isi::PeerInstruction),
    /// Variant of instructions related to `Domain`.
    Domain(crate::domain::isi::DomainInstruction),
    /// Variant of instructions related to `Asset`.
    Asset(crate::asset::isi::AssetInstruction),
    /// Variant of instructions related to `Account`.
    Account(crate::account::isi::AccountInstruction),
    /// This variant of Iroha Special Instructions composes two other instructions into one, and
    /// executes them both.
    Compose(Box<Instruction>, Box<Instruction>),
}

impl Instruction {
    /// Defines the type of the underlying instructions and executes them on `WorldStateView`.
    pub fn execute(&self, world_state_view: &mut WorldStateView) -> Result<(), String> {
        match self {
            Instruction::Peer(origin) => Ok(origin.execute(world_state_view)?),
            Instruction::Domain(origin) => Ok(origin.execute(world_state_view)?),
            Instruction::Asset(origin) => Ok(origin.execute(world_state_view)?),
            Instruction::Account(origin) => Ok(origin.execute(world_state_view)?),
            Instruction::Compose(left, right) => {
                left.execute(world_state_view)?;
                right.execute(world_state_view)?;
                Ok(())
            }
        }
    }
}

/// Generic instruction for an addition of an object to the identifiable destination.
pub struct Add<D, O>
where
    D: Identifiable,
{
    /// Object which should be added.
    pub object: O,
    /// Destination object `Id`.
    pub destination_id: D::Id,
}

impl<D, O> Add<D, O>
where
    D: Identifiable,
{
    /// Default `Add` constructor.
    pub fn new(object: O, destination_id: D::Id) -> Self {
        Add {
            object,
            destination_id,
        }
    }
}

/// Generic instruction for a registration of an object to the identifiable destination.
pub struct Register<D, O>
where
    D: Identifiable,
{
    /// Object which should be registered.
    pub object: O,
    /// Destination object `Id`.
    pub destination_id: D::Id,
}

impl<D, O> Register<D, O>
where
    D: Identifiable,
{
    /// Default `Register` constructor.
    pub fn new(object: O, destination_id: D::Id) -> Self {
        Register {
            object,
            destination_id,
        }
    }
}

/// Generic instruction for a mint of an object to the identifiable destination.
pub struct Mint<D, O>
where
    D: Identifiable,
{
    /// Object which should be minted.
    pub object: O,
    /// Destination object `Id`.
    pub destination_id: D::Id,
}

impl<D, O> Mint<D, O>
where
    D: Identifiable,
{
    /// Default `Mint` constructor.
    pub fn new(object: O, destination_id: D::Id) -> Self {
        Mint {
            object,
            destination_id,
        }
    }
}

/// Generic instruction for a transfer of an object from the identifiable source to the identifiable destination.
pub struct Transfer<Src: Identifiable, Obj, Dst: Identifiable> {
    /// Source object `Id`.
    pub source_id: Src::Id,
    /// Object which should be transfered.
    pub object: Obj,
    /// Destination object `Id`.
    pub destination_id: Dst::Id,
}

impl<Src: Identifiable, Obj, Dst: Identifiable> Transfer<Src, Obj, Dst> {
    /// Default `Transfer` constructor.
    pub fn new(source_id: Src::Id, object: Obj, destination_id: Dst::Id) -> Self {
        Transfer {
            source_id,
            object,
            destination_id,
        }
    }
}
