use crate::error::Error;
use nostr_types::{EncryptedPrivateKey, Event, KeySecurity, PreEvent, PrivateKey, PublicKey};

pub enum Signer {
    Fresh,
    Encrypted(EncryptedPrivateKey),
    Ready(PrivateKey),
}

impl Default for Signer {
    fn default() -> Signer {
        Signer::Fresh
    }
}

impl Signer {
    pub fn load_encrypted_private_key(&mut self, epk: EncryptedPrivateKey) {
        *self = Signer::Encrypted(epk);
    }

    pub fn unlock_encrypted_private_key(&mut self, pass: &str) -> Result<(), Error> {
        if let Signer::Encrypted(epk) = self {
            *self = Signer::Ready(epk.decrypt(pass)?);
            Ok(())
        } else {
            Err(Error::NoPrivateKey)
        }
    }

    pub fn generate_private_key(&mut self, pass: &str) -> Result<EncryptedPrivateKey, Error> {
        *self = Signer::Ready(PrivateKey::generate());
        if let Signer::Ready(pk) = self {
            Ok(pk.export_encrypted(pass)?)
        } else {
            Err(Error::NoPrivateKey)
        }
    }

    pub fn is_loaded(&self) -> bool {
        matches!(self, Signer::Encrypted(_)) || matches!(self, Signer::Ready(_))
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Signer::Ready(_))
    }

    pub fn public_key(&self) -> Option<PublicKey> {
        if let Signer::Ready(pk) = self {
            Some(pk.public_key())
        } else {
            None
        }
    }

    pub fn key_security(&self) -> Option<KeySecurity> {
        if let Signer::Ready(pk) = self {
            Some(pk.key_security())
        } else {
            None
        }
    }

    pub fn sign_preevent(&self, preevent: PreEvent, pow: Option<u8>) -> Result<Event, Error> {
        match self {
            Signer::Ready(pk) => match pow {
                Some(pow) => Ok(Event::new_with_pow(preevent, pk, pow)?),
                None => Ok(Event::new(preevent, pk)?),
            },
            _ => Err(Error::NoPrivateKey),
        }
    }
}
