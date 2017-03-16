#[macro_use]
extern crate exonum;
#[macro_use]
extern crate log;

extern crate serde;
extern crate serde_json;

use std::fmt;

use serde::{Serialize, Serializer};

use exonum::blockchain::{Service, Transaction, Schema, NodeState};
use exonum::node::State;
use exonum::crypto::{PublicKey, Hash, HASH_SIZE};
use exonum::messages::{RawMessage, Message, FromRaw, RawTransaction, Error as MessageError};
use exonum::storage::{StorageValue, Map, View, MapTable, MerklePatriciaTable,
                      Result as StorageResult};
use exonum::blockchain::StoredConfiguration;

pub const CONFIG_SERVICE: u16 = 1;
pub const CONFIG_PROPOSE_MESSAGE_ID: u16 = 0;
pub const CONFIG_VOTE_MESSAGE_ID: u16 = 1;

message! {
    TxConfigPropose {
        const TYPE = CONFIG_SERVICE;
        const ID = CONFIG_PROPOSE_MESSAGE_ID;
        const SIZE = 72;

        from:           &PublicKey  [00 => 32]
        prev_cfg_hash:  &Hash       [32 => 64]
        cfg:            &[u8]       [64 => 72] // serialized config bytes
    }
}

message! {
    TxConfigVote {
        const TYPE = CONFIG_SERVICE;
        const ID = CONFIG_VOTE_MESSAGE_ID;
        const SIZE = 64;

        from:           &PublicKey  [00 => 32]
        cfg_hash:       &Hash       [32 => 64] // hash of config we're voting for
    }
}

#[derive(Clone, PartialEq)]
pub enum ConfigTx {
    ConfigPropose(TxConfigPropose),
    ConfigVote(TxConfigVote),
}

impl Serialize for TxConfigPropose {
    fn serialize<S>(&self, ser: &mut S) -> Result<(), S::Error>
        where S: Serializer
    {
        let mut state;
        state = ser.serialize_struct("config_propose", 4)?;
        ser.serialize_struct_elt(&mut state, "from", self.from())?;
        ser.serialize_struct_elt(&mut state, "previous_config_hash", self.prev_cfg_hash())?;
        if let Ok(cfg) = StoredConfiguration::deserialize_err(self.cfg()){
           ser.serialize_struct_elt(&mut state, "config", cfg)?;
        } else {
           ser.serialize_struct_elt(&mut state, "config", self.cfg())?;
        }
        ser.serialize_struct_end(state)
    }
}

impl Serialize for TxConfigVote {
    fn serialize<S>(&self, ser: &mut S) -> Result<(), S::Error>
        where S: Serializer
    {
        let mut state;
        state = ser.serialize_struct("vote", 5)?;
        ser.serialize_struct_elt(&mut state, "from", self.from())?;
        ser.serialize_struct_elt(&mut state, "config_hash", self.cfg_hash())?;
        ser.serialize_struct_end(state)
    }
}

impl Serialize for ConfigTx {
    fn serialize<S>(&self, ser: &mut S) -> Result<(), S::Error>
        where S: Serializer
    {
        match *self {
            ConfigTx::ConfigPropose(ref propose) => propose.serialize(ser),
            ConfigTx::ConfigVote(ref vote) => vote.serialize(ser),
        }
    }
}

#[derive(Default)]
pub struct ConfigurationService {}

pub struct ConfigurationSchema<'a> {
    view: &'a View,
}

impl ConfigTx {
    pub fn from(&self) -> &PublicKey {
        match *self {
            ConfigTx::ConfigPropose(ref msg) => msg.from(),
            ConfigTx::ConfigVote(ref msg) => msg.from(),
        }
    }
}

impl fmt::Debug for ConfigTx {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self {
            ConfigTx::ConfigPropose(ref msg) => write!(fmt, "{:?}", msg),
            ConfigTx::ConfigVote(ref msg) => write!(fmt, "{:?}", msg),
        }
    }
}

impl FromRaw for ConfigTx {
    fn from_raw(raw: RawMessage) -> Result<ConfigTx, MessageError> {
        match raw.message_type() {
            CONFIG_PROPOSE_MESSAGE_ID => {
                Ok(ConfigTx::ConfigPropose(TxConfigPropose::from_raw(raw)?))
            }
            CONFIG_VOTE_MESSAGE_ID => Ok(ConfigTx::ConfigVote(TxConfigVote::from_raw(raw)?)),
            _ => Err(MessageError::IncorrectMessageType { message_type: raw.message_type() }),
        }
    }
}

impl Message for ConfigTx {
    fn raw(&self) -> &RawMessage {
        match *self {
            ConfigTx::ConfigPropose(ref msg) => msg.raw(),
            ConfigTx::ConfigVote(ref msg) => msg.raw(),
        }
    }

    fn verify_signature(&self, public_key: &PublicKey) -> bool {
        match *self {
            ConfigTx::ConfigPropose(ref msg) => msg.verify_signature(public_key),
            ConfigTx::ConfigVote(ref msg) => msg.verify_signature(public_key),
        }
    }

    fn hash(&self) -> Hash {
        match *self {
            ConfigTx::ConfigPropose(ref msg) =>Message::hash(msg),
            ConfigTx::ConfigVote(ref msg) => Message::hash(msg),
        }
    }
}

impl<'a> ConfigurationSchema<'a> {
    pub fn new(view: &'a View) -> ConfigurationSchema {
        ConfigurationSchema { view: view }
    }

    /// mapping Hash(config) -> TxConfigPropose
    pub fn config_proposes
        (&self)
         -> MerklePatriciaTable<MapTable<View, [u8], Vec<u8>>, Hash, TxConfigPropose> {
        MerklePatriciaTable::new(MapTable::new(vec![04], self.view))
    }

    /// mapping Validator_public_key -> TxConfigVote
    pub fn config_votes
        (&self, config_hash: Hash)
         -> MerklePatriciaTable<MapTable<View, [u8], Vec<u8>>, PublicKey, TxConfigVote> {
        let mut prefix = vec![05; 1 + HASH_SIZE];
        prefix[1..].copy_from_slice(config_hash.as_ref());
        MerklePatriciaTable::new(MapTable::new(prefix, self.view))
    }


    pub fn state_hash(&self) -> StorageResult<Vec<Hash>> {
        Ok(Vec::new())
    }
}

impl TxConfigPropose {
    fn execute(&self, view: &View) -> StorageResult<()> {
        let blockchain_schema = Schema::new(view);
        let config_schema = ConfigurationSchema::new(view);

        let following_config: Option<StoredConfiguration> = blockchain_schema.get_following_configuration()?;
        if let Some(foll_cfg) = following_config {
            let self_repr = serde_json::to_string(self)?;
            error!("Discarding TxConfigPropose: {} as there is an already scheduled next config: {:?} ", self_repr, foll_cfg);
            return Ok(());
        }

        let actual_config: StoredConfiguration = blockchain_schema.get_actual_configuration()?;
        let actual_config_hash = actual_config.hash();
        if *self.prev_cfg_hash() != actual_config_hash {
            let self_repr = serde_json::to_string(self)?;
            error!("Discarding TxConfigPropose:{} which does not reference actual config: {:?}", self_repr, actual_config);
            return Ok(());
        }

        if !actual_config.validators.contains(self.from()) {
            let self_repr = serde_json::to_string(self)?;
            error!("Discarding TxConfigPropose:{} from unknown validator. ", self_repr);
            return Ok(());
        }

        let config_candidate = StoredConfiguration::deserialize_err(self.cfg());
        if config_candidate.is_err() {
            let self_repr = serde_json::to_string(self)?;
            error!("Discarding TxConfigPropose:{} which contains config, which cannot be parsed: {:?}", self_repr, config_candidate);
            return Ok(());
        }

        let config_candidate_body = config_candidate.unwrap();
        let current_height = blockchain_schema.last_height()? + 1;
        let actual_from = config_candidate_body.actual_from;
        if actual_from <= current_height {
            let self_repr = serde_json::to_string(self)?;
            error!("Discarding TxConfigPropose:{} which has actual_from height less than or equal to current: {:?}", self_repr, current_height);
            return Ok(());
        }

        let config_hash = config_candidate_body.hash();

        if let Some(tx_propose) = config_schema.config_proposes().get(&config_hash)? {
            let self_repr = serde_json::to_string(self)?;
            let another_propose_repr = serde_json::to_string(&tx_propose)?;
            error!("Discarding TxConfigPropose:{} which contains an already posted config. Previous TxConfigPropose:{}",
                   self_repr, another_propose_repr);
            return Ok(());
        }

        config_schema.config_proposes().put(&config_hash, self.clone())?;

        let self_repr = serde_json::to_string(self)?;
        debug!("Put TxConfigPropose:{} to config_proposes table", self_repr);
        Ok(())
    }
}

impl TxConfigVote {
    fn execute(&self, view: &View) -> StorageResult<()> {
        let blockchain_schema = Schema::new(view);
        let config_schema = ConfigurationSchema::new(view);

        let propose_option = config_schema.config_proposes().get(self.cfg_hash())?;
        if propose_option.is_none() {
            error!("Discarding TxConfigVote:{:?} which references unknown config hash", self);
            return Ok(());
        }

        let following_config: Option<StoredConfiguration> = blockchain_schema.get_following_configuration()?;
        if let Some(foll_cfg) = following_config {
            error!("Discarding TxConfigVote: {:?} as there is an already scheduled next config: {:?} ", self, foll_cfg);
            return Ok(());
        }

        let referenced_tx_propose = propose_option.unwrap();
        let actual_config: StoredConfiguration = blockchain_schema.get_actual_configuration()?;
        let actual_config_hash = actual_config.hash();
        if *referenced_tx_propose.prev_cfg_hash() != actual_config_hash {
            let propose_repr = serde_json::to_string(&referenced_tx_propose)?;
            error!("Discarding TxConfigVote:{:?}, whose corresponding TxConfigPropose:{} does not reference actual config: {:?}", self, propose_repr, actual_config);
            return Ok(());
        }

        if !actual_config.validators.contains(self.from()) {
            error!("Discarding TxConfigVote:{:?} from unknown validator. ", self.from());
            return Ok(());
        }

        let current_height = blockchain_schema.last_height()? + 1;
        let parsed_config =StoredConfiguration::deserialize_err(referenced_tx_propose.cfg()).unwrap();
        let actual_from = parsed_config.actual_from;
        if actual_from <= current_height {
            let propose_repr = serde_json::to_string(&referenced_tx_propose)?;
            error!("Discarding TxConfigVote:{:?}, whose corresponding TxConfigPropose:{} has actual_from height less than or equal to current: {:?}", self, propose_repr,  current_height);
            return Ok(());
        }

        let votes_for_cfg = config_schema.config_votes(*self.cfg_hash());
        votes_for_cfg.put(self.from(), self.clone())?;
        debug!("Put TxConfigVote:{:?} to corresponding cfg config_votes table", self);

        let mut votes_count = 0;
        for pub_key in &actual_config.validators {
            if let Some(_) = votes_for_cfg.get(&pub_key)? {
                    votes_count += 1;
            }
        }

        if votes_count >= State::byzantine_majority_count(actual_config.validators.len()) {
            blockchain_schema.commit_actual_configuration(parsed_config)?;
        }
        Ok(())
    }
}

impl Transaction for ConfigTx {
    fn verify(&self) -> bool {
        self.verify_signature(self.from())
    }

    fn execute(&self, view: &View) -> StorageResult<()> {
        match *self {
            ConfigTx::ConfigPropose(ref tx) => tx.execute(view),
            ConfigTx::ConfigVote(ref tx) => tx.execute(view),
        }
    }
}

impl ConfigurationService {
    pub fn new() -> ConfigurationService {
        ConfigurationService {}
    }
}

impl Service for ConfigurationService {
    fn service_id(&self) -> u16 {
        CONFIG_SERVICE
    }

    fn state_hash(&self, view: &View) -> StorageResult<Vec<Hash>> {
        let schema = ConfigurationSchema::new(view);
        schema.state_hash()
    }

    fn tx_from_raw(&self, raw: RawTransaction) -> Result<Box<Transaction>, MessageError> {
        ConfigTx::from_raw(raw).map(|tx| Box::new(tx) as Box<Transaction>)
    }

    fn handle_commit(&self, state: &mut NodeState) -> StorageResult<()> {
        let old_cfg = state.actual_config().clone();

        let new_cfg = {
            let schema = Schema::new(state.view());
            schema.get_actual_configuration()?
        };

        if new_cfg != old_cfg {
            info!("Updated node config={:#?}", new_cfg);
            state.update_config(new_cfg);
        }
        Ok(())
    }
}