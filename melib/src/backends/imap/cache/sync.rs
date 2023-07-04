/*
 * melib - IMAP
 *
 * Copyright 2020 Manos Pitsidianakis
 *
 * This file is part of meli.
 *
 * meli is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * meli is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with meli. If not, see <http://www.gnu.org/licenses/>.
 */

use imap_codec::{
    fetch::{MacroOrMessageDataItemNames, MessageDataItemName},
    search::SearchKey,
    sequence::SequenceSet,
    status::StatusDataItemName,
};

use super::*;

impl ImapConnection {
    pub async fn resync(&mut self, mailbox_hash: MailboxHash) -> Result<Option<Vec<Envelope>>> {
        debug!("resync mailbox_hash {}", mailbox_hash);
        debug!(&self.sync_policy);
        if matches!(self.sync_policy, SyncPolicy::None) {
            return Ok(None);
        }

        #[cfg(not(feature = "sqlite3"))]
        let mut cache_handle = DefaultCache::get(self.uid_store.clone())?;
        #[cfg(feature = "sqlite3")]
        let mut cache_handle = Sqlite3Cache::get(self.uid_store.clone())?;
        if cache_handle.mailbox_state(mailbox_hash)?.is_none() {
            return Ok(None);
        }

        match self.sync_policy {
            SyncPolicy::None => Ok(None),
            SyncPolicy::Basic => self.resync_basic(cache_handle, mailbox_hash).await,
            SyncPolicy::Condstore => self.resync_condstore(cache_handle, mailbox_hash).await,
            SyncPolicy::CondstoreQresync => {
                self.resync_condstoreqresync(cache_handle, mailbox_hash)
                    .await
            }
        }
    }

    pub async fn load_cache(
        &mut self,
        mailbox_hash: MailboxHash,
    ) -> Option<Result<Vec<EnvelopeHash>>> {
        debug!("load_cache {}", mailbox_hash);
        #[cfg(not(feature = "sqlite3"))]
        let mut cache_handle = match DefaultCache::get(self.uid_store.clone()) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        #[cfg(feature = "sqlite3")]
        let mut cache_handle = match Sqlite3Cache::get(self.uid_store.clone()) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        match cache_handle.mailbox_state(mailbox_hash) {
            Err(err) => return Some(Err(err)),
            Ok(Some(())) => {}
            Ok(None) => {
                return None;
            }
        };
        match cache_handle.envelopes(mailbox_hash) {
            Ok(Some(envs)) => Some(Ok(envs)),
            Ok(None) => None,
            Err(err) => Some(Err(err)),
        }
    }

    //rfc4549_Synchronization_Operations_for_Disconnected_IMAP4_Clients
    pub async fn resync_basic(
        &mut self,
        mut cache_handle: Box<dyn ImapCache>,
        mailbox_hash: MailboxHash,
    ) -> Result<Option<Vec<Envelope>>> {
        let mut payload = vec![];
        debug!("resync_basic");
        let mut response = Vec::with_capacity(8 * 1024);
        let cached_uidvalidity = self
            .uid_store
            .uidvalidity
            .lock()
            .unwrap()
            .get(&mailbox_hash)
            .cloned();
        let cached_max_uid = self
            .uid_store
            .max_uids
            .lock()
            .unwrap()
            .get(&mailbox_hash)
            .cloned();
        // 3. tag2 UID FETCH 1:<lastseenuid> FLAGS
        //if cached_uidvalidity.is_none() || cached_max_uid.is_none() {
        //    return Ok(None);
        //}

        let current_uidvalidity: UID = cached_uidvalidity.unwrap_or(1);
        let max_uid: UID = cached_max_uid.unwrap_or(1);
        let (mailbox_path, mailbox_exists, unseen) = {
            let f = &self.uid_store.mailboxes.lock().await[&mailbox_hash];
            (
                f.imap_path().to_string(),
                f.exists.clone(),
                f.unseen.clone(),
            )
        };
        let mut new_unseen = BTreeSet::default();
        let select_response = self
            .select_mailbox(mailbox_hash, &mut response, true)
            .await?
            .unwrap();
        // 1. check UIDVALIDITY. If fail, discard cache and rebuild
        if select_response.uidvalidity != current_uidvalidity {
            cache_handle.clear(mailbox_hash, &select_response)?;
            return Ok(None);
        }
        cache_handle.update_mailbox(mailbox_hash, &select_response)?;

        // 2. tag1 UID FETCH <lastseenuid+1>:* <descriptors>
        self.send_command(CommandBody::fetch(
            max_uid + 1..,
            common_attributes(),
            true,
        )?)
        .await?;
        self.read_response(&mut response, RequiredResponses::FETCH_REQUIRED)
            .await?;
        debug!(
            "fetch response is {} bytes and {} lines",
            response.len(),
            String::from_utf8_lossy(&response).lines().count()
        );
        let (_, mut v, _) = protocol_parser::fetch_responses(&response)?;
        debug!("responses len is {}", v.len());
        for FetchResponse {
            ref uid,
            ref mut envelope,
            ref mut flags,
            ref references,
            ..
        } in v.iter_mut()
        {
            let uid = uid.unwrap();
            let env = envelope.as_mut().unwrap();
            env.set_hash(generate_envelope_hash(&mailbox_path, &uid));
            if let Some(value) = references {
                env.set_references(value);
            }
            let mut tag_lck = self.uid_store.collection.tag_index.write().unwrap();
            if let Some((flags, keywords)) = flags {
                env.set_flags(*flags);
                if !env.is_seen() {
                    new_unseen.insert(env.hash());
                }
                for f in keywords {
                    let hash = TagHash::from_bytes(f.as_bytes());
                    tag_lck.entry(hash).or_insert_with(|| f.to_string());
                    env.tags_mut().push(hash);
                }
            }
        }
        {
            cache_handle
                .insert_envelopes(mailbox_hash, &v)
                .chain_err_summary(|| {
                    format!(
                        "Could not save envelopes in cache for mailbox {}",
                        mailbox_path
                    )
                })?;
        }

        for FetchResponse {
            uid,
            message_sequence_number: _,
            envelope,
            ..
        } in v
        {
            let uid = uid.unwrap();
            let env = envelope.unwrap();
            /*
            debug!(
                "env hash {} {} UID = {} MSN = {}",
                env.hash(),
                env.subject(),
                uid,
                message_sequence_number
            );
            */
            self.uid_store
                .hash_index
                .lock()
                .unwrap()
                .insert(env.hash(), (uid, mailbox_hash));
            self.uid_store
                .uid_index
                .lock()
                .unwrap()
                .insert((mailbox_hash, uid), env.hash());
            payload.push((uid, env));
        }
        debug!("sending payload for {}", mailbox_hash);
        let payload_hash_set: BTreeSet<_> =
            payload.iter().map(|(_, env)| env.hash()).collect::<_>();
        {
            let mut unseen_lck = unseen.lock().unwrap();
            for &seen_env_hash in payload_hash_set.difference(&new_unseen) {
                unseen_lck.remove(seen_env_hash);
            }

            unseen_lck.insert_set(new_unseen);
        }
        mailbox_exists.lock().unwrap().insert_set(payload_hash_set);
        // 3. tag2 UID FETCH 1:<lastseenuid> FLAGS
        let sequence_set = if max_uid == 0 {
            SequenceSet::from(..)
        } else {
            SequenceSet::try_from(..=max_uid)?
        };
        self.send_command(CommandBody::Fetch {
            sequence_set,
            macro_or_item_names: MacroOrMessageDataItemNames::MessageDataItemNames(vec![
                MessageDataItemName::Flags,
            ]),
            uid: true,
        })
        .await?;
        self.read_response(&mut response, RequiredResponses::FETCH_REQUIRED)
            .await?;
        // 1) update cached flags for old messages;
        // 2) find out which old messages got expunged; and
        // 3) build a mapping between message numbers and UIDs (for old messages).
        let mut valid_envs = BTreeSet::default();
        let mut env_lck = self.uid_store.envelopes.lock().unwrap();
        let (_, v, _) = protocol_parser::fetch_responses(&response)?;
        let mut refresh_events = vec![];
        for FetchResponse { uid, flags, .. } in v {
            let uid = uid.unwrap();
            let env_hash = generate_envelope_hash(&mailbox_path, &uid);
            valid_envs.insert(env_hash);
            if !env_lck.contains_key(&env_hash) {
                return Ok(None);
            }
            let (flags, tags) = flags.unwrap();
            if env_lck[&env_hash].inner.flags() != flags
                || env_lck[&env_hash].inner.tags()
                    != &tags
                        .iter()
                        .map(|t| TagHash::from_bytes(t.as_bytes()))
                        .collect::<SmallVec<[TagHash; 8]>>()
            {
                env_lck.entry(env_hash).and_modify(|entry| {
                    entry.inner.set_flags(flags);
                    entry.inner.tags_mut().clear();
                    entry
                        .inner
                        .tags_mut()
                        .extend(tags.iter().map(|t| TagHash::from_bytes(t.as_bytes())));
                });
                refresh_events.push((
                    uid,
                    RefreshEvent {
                        mailbox_hash,
                        account_hash: self.uid_store.account_hash,
                        kind: RefreshEventKind::NewFlags(env_hash, (flags, tags)),
                    },
                ));
            }
        }
        for env_hash in env_lck
            .iter()
            .filter_map(|(h, cenv)| {
                if cenv.mailbox_hash == mailbox_hash {
                    Some(*h)
                } else {
                    None
                }
            })
            .collect::<BTreeSet<EnvelopeHash>>()
            .difference(&valid_envs)
        {
            refresh_events.push((
                env_lck[env_hash].uid,
                RefreshEvent {
                    mailbox_hash,
                    account_hash: self.uid_store.account_hash,
                    kind: RefreshEventKind::Remove(*env_hash),
                },
            ));
            env_lck.remove(env_hash);
        }
        drop(env_lck);
        cache_handle.update(mailbox_hash, &refresh_events)?;
        for (_uid, ev) in refresh_events {
            self.add_refresh_event(ev);
        }
        Ok(Some(payload.into_iter().map(|(_, env)| env).collect()))
    }

    //rfc4549_Synchronization_Operations_for_Disconnected_IMAP4_Clients
    //Section 6.1
    pub async fn resync_condstore(
        &mut self,
        mut cache_handle: Box<dyn ImapCache>,
        mailbox_hash: MailboxHash,
    ) -> Result<Option<Vec<Envelope>>> {
        let mut payload = vec![];
        debug!("resync_condstore");
        let mut response = Vec::with_capacity(8 * 1024);
        let cached_uidvalidity = self
            .uid_store
            .uidvalidity
            .lock()
            .unwrap()
            .get(&mailbox_hash)
            .cloned();
        let cached_max_uid = self
            .uid_store
            .max_uids
            .lock()
            .unwrap()
            .get(&mailbox_hash)
            .cloned();
        let cached_highestmodseq = self
            .uid_store
            .highestmodseqs
            .lock()
            .unwrap()
            .get(&mailbox_hash)
            .cloned();
        if cached_uidvalidity.is_none()
            || cached_max_uid.is_none()
            || cached_highestmodseq.is_none()
        {
            // This means the mailbox is not cached.
            return Ok(None);
        }
        let cached_uidvalidity: UID = cached_uidvalidity.unwrap();
        let cached_max_uid: UID = cached_max_uid.unwrap();
        let cached_highestmodseq: std::result::Result<ModSequence, ()> =
            cached_highestmodseq.unwrap();
        if cached_highestmodseq.is_err() {
            // No MODSEQ is available for __this__ mailbox, fallback to basic sync
            return self.resync_basic(cache_handle, mailbox_hash).await;
        }
        let cached_highestmodseq: ModSequence = cached_highestmodseq.unwrap();

        let (mailbox_path, mailbox_exists, unseen) = {
            let f = &self.uid_store.mailboxes.lock().await[&mailbox_hash];
            (
                f.imap_path().to_string(),
                f.exists.clone(),
                f.unseen.clone(),
            )
        };
        let mut new_unseen = BTreeSet::default();
        // 1. check UIDVALIDITY. If fail, discard cache and rebuild
        let select_response = self
            .select_mailbox(mailbox_hash, &mut response, true)
            .await?
            .unwrap();
        if select_response.uidvalidity != cached_uidvalidity {
            // 1a) Check the mailbox UIDVALIDITY (see section 4.1 for more
            //details) with SELECT/EXAMINE/STATUS.
            //   If the UIDVALIDITY value returned by the server differs, the
            //   client MUST
            //   * empty the local cache of that mailbox;
            //   * "forget" the cached HIGHESTMODSEQ value for the mailbox;
            //   * remove any pending "actions" that refer to UIDs in that mailbox (note
            //     that this doesn't affect actions performed on client-generated fake UIDs;
            //     see Section 5); and
            //   * skip steps 1b and 2-II;
            cache_handle.clear(mailbox_hash, &select_response)?;
            return Ok(None);
        }
        if select_response.highestmodseq.is_none()
            || select_response.highestmodseq.as_ref().unwrap().is_err()
        {
            if select_response.highestmodseq.as_ref().unwrap().is_err() {
                self.uid_store
                    .highestmodseqs
                    .lock()
                    .unwrap()
                    .insert(mailbox_hash, Err(()));
            }
            return self.resync_basic(cache_handle, mailbox_hash).await;
        }
        cache_handle.update_mailbox(mailbox_hash, &select_response)?;
        let new_highestmodseq = select_response.highestmodseq.unwrap().unwrap();
        let mut refresh_events = vec![];
        // 1b) Check the mailbox HIGHESTMODSEQ.
        //  If the cached value is the same as the one returned by the server, skip
        // fetching  message flags on step 2-II, i.e., the client only has to
        // find out which messages got  expunged.
        if cached_highestmodseq != new_highestmodseq {
            /* Cache is synced, only figure out which messages got expunged */

            // 2) Fetch the current "descriptors".
            //   I)  Discover new messages.

            //   II) Discover changes to old messages and flags for new messages
            //       using
            //       "FETCH 1:* (FLAGS) (CHANGEDSINCE <cached-value>)" or
            //       "SEARCH MODSEQ <cached-value>".

            // 2. tag1 UID FETCH <lastseenuid+1>:* <descriptors>
            // TODO(#222): imap-codec does not support "CONDSTORE/QRESYNC" currently.
            self.send_command_raw(
                format!(
                    "UID FETCH {}:* (UID FLAGS ENVELOPE BODY.PEEK[HEADER.FIELDS (REFERENCES)] \
                     BODYSTRUCTURE) (CHANGEDSINCE {})",
                    cached_max_uid + 1,
                    cached_highestmodseq,
                )
                .as_bytes(),
            )
            .await?;
            self.read_response(&mut response, RequiredResponses::FETCH_REQUIRED)
                .await?;
            debug!(
                "fetch response is {} bytes and {} lines",
                response.len(),
                String::from_utf8_lossy(&response).lines().count()
            );
            let (_, mut v, _) = protocol_parser::fetch_responses(&response)?;
            debug!("responses len is {}", v.len());
            for FetchResponse {
                ref uid,
                ref mut envelope,
                ref mut flags,
                ref references,
                ..
            } in v.iter_mut()
            {
                let uid = uid.unwrap();
                let env = envelope.as_mut().unwrap();
                env.set_hash(generate_envelope_hash(&mailbox_path, &uid));
                if let Some(value) = references {
                    env.set_references(value);
                }
                let mut tag_lck = self.uid_store.collection.tag_index.write().unwrap();
                if let Some((flags, keywords)) = flags {
                    env.set_flags(*flags);
                    if !env.is_seen() {
                        new_unseen.insert(env.hash());
                    }
                    for f in keywords {
                        let hash = TagHash::from_bytes(f.as_bytes());
                        tag_lck.entry(hash).or_insert_with(|| f.to_string());
                        env.tags_mut().push(hash);
                    }
                }
            }
            {
                cache_handle
                    .insert_envelopes(mailbox_hash, &v)
                    .chain_err_summary(|| {
                        format!(
                            "Could not save envelopes in cache for mailbox {}",
                            mailbox_path
                        )
                    })?;
            }

            for FetchResponse { uid, envelope, .. } in v {
                let uid = uid.unwrap();
                let env = envelope.unwrap();
                /*
                debug!(
                    "env hash {} {} UID = {} MSN = {}",
                    env.hash(),
                    env.subject(),
                    uid,
                    message_sequence_number
                );
                */
                self.uid_store
                    .hash_index
                    .lock()
                    .unwrap()
                    .insert(env.hash(), (uid, mailbox_hash));
                self.uid_store
                    .uid_index
                    .lock()
                    .unwrap()
                    .insert((mailbox_hash, uid), env.hash());
                payload.push((uid, env));
            }
            debug!("sending payload for {}", mailbox_hash);
            let payload_hash_set: BTreeSet<_> =
                payload.iter().map(|(_, env)| env.hash()).collect::<_>();
            {
                let mut unseen_lck = unseen.lock().unwrap();
                for &seen_env_hash in payload_hash_set.difference(&new_unseen) {
                    unseen_lck.remove(seen_env_hash);
                }

                unseen_lck.insert_set(new_unseen);
            }
            mailbox_exists.lock().unwrap().insert_set(payload_hash_set);
            // 3. tag2 UID FETCH 1:<lastseenuid> FLAGS
            if cached_max_uid == 0 {
                // TODO(#222): imap-codec does not support "CONDSTORE/QRESYNC" currently.
                self.send_command_raw(
                    format!(
                        "UID FETCH 1:* FLAGS (CHANGEDSINCE {})",
                        cached_highestmodseq
                    )
                    .as_bytes(),
                )
                .await?;
            } else {
                // TODO(#222): imap-codec does not support "CONDSTORE/QRESYNC" currently.
                self.send_command_raw(
                    format!(
                        "UID FETCH 1:{} FLAGS (CHANGEDSINCE {})",
                        cached_max_uid, cached_highestmodseq
                    )
                    .as_bytes(),
                )
                .await?;
            }
            self.read_response(&mut response, RequiredResponses::FETCH_REQUIRED)
                .await?;
            // 1) update cached flags for old messages;
            let mut env_lck = self.uid_store.envelopes.lock().unwrap();
            let (_, v, _) = protocol_parser::fetch_responses(&response)?;
            for FetchResponse { uid, flags, .. } in v {
                let uid = uid.unwrap();
                let env_hash = generate_envelope_hash(&mailbox_path, &uid);
                if !env_lck.contains_key(&env_hash) {
                    return Ok(None);
                }
                let (flags, tags) = flags.unwrap();
                if env_lck[&env_hash].inner.flags() != flags
                    || env_lck[&env_hash].inner.tags()
                        != &tags
                            .iter()
                            .map(|t| TagHash::from_bytes(t.as_bytes()))
                            .collect::<SmallVec<[TagHash; 8]>>()
                {
                    env_lck.entry(env_hash).and_modify(|entry| {
                        entry.inner.set_flags(flags);
                        entry.inner.tags_mut().clear();
                        entry
                            .inner
                            .tags_mut()
                            .extend(tags.iter().map(|t| TagHash::from_bytes(t.as_bytes())));
                    });
                    refresh_events.push((
                        uid,
                        RefreshEvent {
                            mailbox_hash,
                            account_hash: self.uid_store.account_hash,
                            kind: RefreshEventKind::NewFlags(env_hash, (flags, tags)),
                        },
                    ));
                }
            }
            self.uid_store
                .highestmodseqs
                .lock()
                .unwrap()
                .insert(mailbox_hash, Ok(new_highestmodseq));
        }
        let mut valid_envs = BTreeSet::default();
        // This should be UID SEARCH 1:<maxuid> but it's difficult to compare to cached
        // UIDs at the point of calling this function
        self.send_command(CommandBody::search(None, SearchKey::All, true))
            .await?;
        self.read_response(&mut response, RequiredResponses::SEARCH)
            .await?;
        // 1) update cached flags for old messages;
        let (_, v) = protocol_parser::search_results(response.as_slice())?;
        for uid in v {
            valid_envs.insert(generate_envelope_hash(&mailbox_path, &uid));
        }
        {
            let mut env_lck = self.uid_store.envelopes.lock().unwrap();
            let olds = env_lck
                .iter()
                .filter_map(|(h, cenv)| {
                    if cenv.mailbox_hash == mailbox_hash {
                        Some(*h)
                    } else {
                        None
                    }
                })
                .collect::<BTreeSet<EnvelopeHash>>();
            for env_hash in olds.difference(&valid_envs) {
                refresh_events.push((
                    env_lck[env_hash].uid,
                    RefreshEvent {
                        mailbox_hash,
                        account_hash: self.uid_store.account_hash,
                        kind: RefreshEventKind::Remove(*env_hash),
                    },
                ));
                env_lck.remove(env_hash);
            }
            drop(env_lck);
        }
        cache_handle.update(mailbox_hash, &refresh_events)?;
        for (_uid, ev) in refresh_events {
            self.add_refresh_event(ev);
        }
        Ok(Some(payload.into_iter().map(|(_, env)| env).collect()))
    }

    //rfc7162_Quick Flag Changes Resynchronization (CONDSTORE)_and Quick Mailbox
    // Resynchronization (QRESYNC)
    pub async fn resync_condstoreqresync(
        &mut self,
        _cache_handle: Box<dyn ImapCache>,
        _mailbox_hash: MailboxHash,
    ) -> Result<Option<Vec<Envelope>>> {
        Ok(None)
    }

    pub async fn init_mailbox(&mut self, mailbox_hash: MailboxHash) -> Result<SelectResponse> {
        let mut response = Vec::with_capacity(8 * 1024);
        let (mailbox_path, mailbox_exists, permissions) = {
            let f = &self.uid_store.mailboxes.lock().await[&mailbox_hash];
            (
                f.imap_path().to_string(),
                f.exists.clone(),
                f.permissions.clone(),
            )
        };

        /* first SELECT the mailbox to get READ/WRITE permissions (because EXAMINE
         * only returns READ-ONLY for both cases) */
        let mut select_response = self
            .select_mailbox(mailbox_hash, &mut response, true)
            .await?
            .unwrap();
        debug!(
            "mailbox: {} select_response: {:?}",
            mailbox_path, select_response
        );
        {
            {
                let mut uidvalidities = self.uid_store.uidvalidity.lock().unwrap();

                let v = uidvalidities
                    .entry(mailbox_hash)
                    .or_insert(select_response.uidvalidity);
                *v = select_response.uidvalidity;
            }
            {
                if let Some(highestmodseq) = select_response.highestmodseq {
                    let mut highestmodseqs = self.uid_store.highestmodseqs.lock().unwrap();
                    let v = highestmodseqs.entry(mailbox_hash).or_insert(highestmodseq);
                    *v = highestmodseq;
                }
            }
            let mut permissions = permissions.lock().unwrap();
            permissions.create_messages = !select_response.read_only;
            permissions.remove_messages = !select_response.read_only;
            permissions.set_flags = !select_response.read_only;
            permissions.rename_messages = !select_response.read_only;
            permissions.delete_messages = !select_response.read_only;
            {
                let mut mailbox_exists_lck = mailbox_exists.lock().unwrap();
                mailbox_exists_lck.clear();
                mailbox_exists_lck.set_not_yet_seen(select_response.exists);
            }
        }
        if select_response.exists == 0 {
            return Ok(select_response);
        }
        /* reselecting the same mailbox with EXAMINE prevents expunging it */
        self.examine_mailbox(mailbox_hash, &mut response, true)
            .await?;
        if select_response.uidnext == 0 {
            /* UIDNEXT shouldn't be 0, since exists != 0 at this point */
            self.send_command(CommandBody::status(
                mailbox_path,
                [StatusDataItemName::UidNext].as_slice(),
            )?)
            .await?;
            self.read_response(&mut response, RequiredResponses::STATUS)
                .await?;
            let (_, status) = protocol_parser::status_response(response.as_slice())?;
            if let Some(uidnext) = status.uidnext {
                if uidnext == 0 {
                    return Err(Error::new(
                        "IMAP server error: zero UIDNEXT with nonzero exists.",
                    ));
                }
                select_response.uidnext = uidnext;
            } else {
                return Err(Error::new("IMAP server did not reply with UIDNEXT"));
            }
        }
        Ok(select_response)
    }
}
