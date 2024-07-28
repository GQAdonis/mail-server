/*
 * SPDX-FileCopyrightText: 2020 Stalwart Labs Ltd <hello@stalw.art>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-SEL
 */

use std::{sync::Arc, time::Instant};

use jmap_proto::{
    method::{
        get, query,
        set::{self},
    },
    request::{method::MethodName, Call, Request, RequestMethod},
    response::{Response, ResponseMethod},
    types::collection::Collection,
};
use trc::JmapEvent;

use crate::{auth::AccessToken, JMAP};

use super::http::HttpSessionData;

impl JMAP {
    pub async fn handle_request(
        &self,
        request: Request,
        access_token: Arc<AccessToken>,
        session: &HttpSessionData,
    ) -> Response {
        let mut response = Response::new(
            access_token.state(),
            request.created_ids.unwrap_or_default(),
            request.method_calls.len(),
        );
        let add_created_ids = !response.created_ids.is_empty();

        for mut call in request.method_calls {
            // Resolve result and id references
            if let Err(error) = response.resolve_references(&mut call.method) {
                let method_error = error.clone();

                trc::error!(error.span_id(session.session_id));

                response.push_response(call.id, MethodName::error(), method_error);
                continue;
            }

            loop {
                let mut next_call = None;

                // Add response
                let method_name = call.name.as_str();
                match self
                    .handle_method_call(
                        call.method,
                        method_name,
                        &access_token,
                        &mut next_call,
                        session,
                    )
                    .await
                {
                    Ok(mut method_response) => {
                        match &mut method_response {
                            ResponseMethod::Set(set_response) => {
                                // Add created ids
                                set_response.update_created_ids(&mut response);

                                // Publish state changes
                                if let Some(state_change) = set_response.state_change.take() {
                                    self.broadcast_state_change(state_change).await;
                                }
                            }
                            ResponseMethod::ImportEmail(import_response) => {
                                // Add created ids
                                import_response.update_created_ids(&mut response);

                                // Publish state changes
                                if let Some(state_change) = import_response.state_change.take() {
                                    self.broadcast_state_change(state_change).await;
                                }
                            }
                            ResponseMethod::Copy(copy_response) => {
                                // Publish state changes
                                if let Some(state_change) = copy_response.state_change.take() {
                                    self.broadcast_state_change(state_change).await;
                                }
                            }
                            ResponseMethod::UploadBlob(upload_response) => {
                                // Add created blobIds
                                upload_response.update_created_ids(&mut response);
                            }
                            _ => {}
                        }

                        response.push_response(call.id, call.name, method_response);
                    }
                    Err(error) => {
                        let method_error = error.clone();

                        trc::error!(error
                            .span_id(session.session_id)
                            .ctx_unique(trc::Key::AccountId, access_token.primary_id())
                            .caused_by(method_name));

                        response.push_error(call.id, method_error);
                    }
                }

                // Process next call
                if let Some(next_call) = next_call {
                    call = next_call;
                    call.id
                        .clone_from(&response.method_responses.last().unwrap().id);
                } else {
                    break;
                }
            }
        }

        if !add_created_ids {
            response.created_ids.clear();
        }

        response
    }

    async fn handle_method_call(
        &self,
        method: RequestMethod,
        method_name: &'static str,
        access_token: &AccessToken,
        next_call: &mut Option<Call<RequestMethod>>,
        session: &HttpSessionData,
    ) -> trc::Result<ResponseMethod> {
        let op_start = Instant::now();
        let response = match method {
            RequestMethod::Get(mut req) => match req.take_arguments() {
                get::RequestArguments::Email(arguments) => {
                    access_token.assert_has_access(req.account_id, Collection::Email)?;

                    self.email_get(req.with_arguments(arguments), access_token)
                        .await?
                        .into()
                }
                get::RequestArguments::Mailbox => {
                    access_token.assert_has_access(req.account_id, Collection::Mailbox)?;

                    self.mailbox_get(req, access_token).await?.into()
                }
                get::RequestArguments::Thread => {
                    access_token.assert_has_access(req.account_id, Collection::Email)?;

                    self.thread_get(req).await?.into()
                }
                get::RequestArguments::Identity => {
                    access_token.assert_is_member(req.account_id)?;

                    self.identity_get(req).await?.into()
                }
                get::RequestArguments::EmailSubmission => {
                    access_token.assert_is_member(req.account_id)?;

                    self.email_submission_get(req).await?.into()
                }
                get::RequestArguments::PushSubscription => {
                    self.push_subscription_get(req, access_token).await?.into()
                }
                get::RequestArguments::SieveScript => {
                    access_token.assert_is_member(req.account_id)?;

                    self.sieve_script_get(req).await?.into()
                }
                get::RequestArguments::VacationResponse => {
                    access_token.assert_is_member(req.account_id)?;

                    self.vacation_response_get(req).await?.into()
                }
                get::RequestArguments::Principal => {
                    if self.core.jmap.principal_allow_lookups || access_token.is_super_user() {
                        self.principal_get(req).await?.into()
                    } else {
                        return Err(trc::JmapEvent::Forbidden
                            .into_err()
                            .details("Principal lookups are disabled".to_string()));
                    }
                }
                get::RequestArguments::Quota => {
                    access_token.assert_is_member(req.account_id)?;

                    self.quota_get(req, access_token).await?.into()
                }
                get::RequestArguments::Blob(arguments) => {
                    access_token.assert_is_member(req.account_id)?;

                    self.blob_get(req.with_arguments(arguments), access_token)
                        .await?
                        .into()
                }
            },
            RequestMethod::Query(mut req) => match req.take_arguments() {
                query::RequestArguments::Email(arguments) => {
                    access_token.assert_has_access(req.account_id, Collection::Email)?;

                    self.email_query(req.with_arguments(arguments), access_token)
                        .await?
                        .into()
                }
                query::RequestArguments::Mailbox(arguments) => {
                    access_token.assert_has_access(req.account_id, Collection::Mailbox)?;

                    self.mailbox_query(req.with_arguments(arguments), access_token)
                        .await?
                        .into()
                }
                query::RequestArguments::EmailSubmission => {
                    access_token.assert_is_member(req.account_id)?;

                    self.email_submission_query(req).await?.into()
                }
                query::RequestArguments::SieveScript => {
                    access_token.assert_is_member(req.account_id)?;

                    self.sieve_script_query(req).await?.into()
                }
                query::RequestArguments::Principal => {
                    if self.core.jmap.principal_allow_lookups || access_token.is_super_user() {
                        self.principal_query(req, session).await?.into()
                    } else {
                        return Err(trc::JmapEvent::Forbidden
                            .into_err()
                            .details("Principal lookups are disabled".to_string()));
                    }
                }
                query::RequestArguments::Quota => {
                    access_token.assert_is_member(req.account_id)?;

                    self.quota_query(req, access_token).await?.into()
                }
            },
            RequestMethod::Set(mut req) => match req.take_arguments() {
                set::RequestArguments::Email => {
                    access_token.assert_has_access(req.account_id, Collection::Email)?;

                    self.email_set(req, access_token, session).await?.into()
                }
                set::RequestArguments::Mailbox(arguments) => {
                    access_token.assert_has_access(req.account_id, Collection::Mailbox)?;

                    self.mailbox_set(req.with_arguments(arguments), access_token)
                        .await?
                        .into()
                }
                set::RequestArguments::Identity => {
                    access_token.assert_is_member(req.account_id)?;

                    self.identity_set(req).await?.into()
                }
                set::RequestArguments::EmailSubmission(arguments) => {
                    access_token.assert_is_member(req.account_id)?;

                    self.email_submission_set(
                        req.with_arguments(arguments),
                        &session.instance,
                        next_call,
                    )
                    .await?
                    .into()
                }
                set::RequestArguments::PushSubscription => {
                    self.push_subscription_set(req, access_token).await?.into()
                }
                set::RequestArguments::SieveScript(arguments) => {
                    access_token.assert_is_member(req.account_id)?;

                    self.sieve_script_set(req.with_arguments(arguments), access_token, session)
                        .await?
                        .into()
                }
                set::RequestArguments::VacationResponse => {
                    access_token.assert_is_member(req.account_id)?;

                    self.vacation_response_set(req).await?.into()
                }
            },
            RequestMethod::Changes(req) => self.changes(req, access_token).await?.into(),
            RequestMethod::Copy(req) => {
                access_token
                    .assert_has_access(req.account_id, Collection::Email)?
                    .assert_has_access(req.from_account_id, Collection::Email)?;

                self.email_copy(req, access_token, next_call, session)
                    .await?
                    .into()
            }
            RequestMethod::ImportEmail(req) => {
                access_token.assert_has_access(req.account_id, Collection::Email)?;

                self.email_import(req, access_token, session).await?.into()
            }
            RequestMethod::ParseEmail(req) => {
                access_token.assert_has_access(req.account_id, Collection::Email)?;

                self.email_parse(req, access_token).await?.into()
            }
            RequestMethod::QueryChanges(req) => self.query_changes(req, access_token).await?.into(),
            RequestMethod::SearchSnippet(req) => {
                access_token.assert_has_access(req.account_id, Collection::Email)?;

                self.email_search_snippet(req, access_token).await?.into()
            }
            RequestMethod::ValidateScript(req) => {
                access_token.assert_is_member(req.account_id)?;

                self.sieve_script_validate(req, access_token).await?.into()
            }
            RequestMethod::CopyBlob(req) => {
                access_token.assert_is_member(req.account_id)?;

                self.blob_copy(req, access_token).await?.into()
            }
            RequestMethod::LookupBlob(req) => {
                access_token.assert_is_member(req.account_id)?;

                self.blob_lookup(req).await?.into()
            }
            RequestMethod::UploadBlob(req) => {
                access_token.assert_is_member(req.account_id)?;

                self.blob_upload_many(req, access_token).await?.into()
            }
            RequestMethod::Echo(req) => req.into(),
            RequestMethod::Error(error) => return Err(error),
        };

        trc::event!(
            Jmap(JmapEvent::MethodCall),
            Name = method_name,
            SpanId = session.session_id,
            AccountId = access_token.primary_id(),
            Elapsed = op_start.elapsed(),
        );

        Ok(response)
    }
}
