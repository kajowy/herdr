use super::*;

impl HeadlessServer {
    pub(super) fn handle_client_shell_endpoint_request(
        &mut self,
        client_id: u64,
        boot_id: String,
        mut request: Box<api::schema::Request>,
    ) -> bool {
        let Some(client) = self.clients.get(&client_id) else {
            return false;
        };
        if !matches!(client.mode, ClientConnectionMode::ClientShell) {
            self.remove_client_and_resize_if_needed(client_id);
            return true;
        }
        let request_id = request.id.clone();
        if !crate::server::client_commands::supports_client_shell_method(&request.method) {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "unsupported_endpoint_command",
                "this method is not available through the client shell command lane",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        if boot_id != self.client_shell_boot_id {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "stale_boot",
                "endpoint command targeted an earlier server boot",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        let surface_active = client.shell_surface_active;
        if let api::schema::Method::ClientShellSurfaceSet(params) = &request.method {
            let Some((changed, projection_revision)) =
                self.set_client_shell_surface_active(client_id, params.active)
            else {
                return false;
            };
            self.send_to_client(
                client_id,
                crate::server::client_commands::success_message_with_result(
                    boot_id,
                    request_id,
                    api::schema::ResponseResult::ClientShellSurfaceSet {
                        active: params.active,
                        projection_revision,
                    },
                ),
            );
            return changed;
        }
        if client.shell_endpoint_command_in_flight {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "endpoint_busy",
                "this endpoint is still processing another command",
            );
            self.send_to_client(client_id, message);
            return false;
        }
        if !surface_active {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "surface_inactive",
                "this method requires an active client shell surface",
            );
            self.send_to_client(client_id, message);
            return false;
        }

        if matches!(
            &request.method,
            api::schema::Method::FilePutBegin(_)
                | api::schema::Method::FilePutChunk(_)
                | api::schema::Method::FilePutCommit(_)
                | api::schema::Method::FilePutAbort(_)
        ) {
            return self.handle_file_put_request(client_id, boot_id, request_id, &request.method);
        }

        let api_request_id = format!(
            "endpoint:{}:{client_id}:{request_id}",
            self.client_shell_boot_id
        );
        request.id = api_request_id.clone();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        if let Err(err) = crate::server::client_commands::spawn_response_waiter(
            client_id,
            boot_id.clone(),
            request_id.clone(),
            response_rx,
            self.server_event_tx.clone(),
        ) {
            let message = crate::server::client_commands::error_message(
                boot_id,
                request_id,
                "server_unavailable",
                format!("failed to start endpoint response bridge: {err}"),
            );
            self.send_to_client(client_id, message);
            return false;
        }
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.shell_endpoint_command_in_flight = true;
            // A later source restore has a new projection revision. Keep this request's lease
            // so a delayed worktree response cannot focus a pane after endpoint switching.
            client.shell_endpoint_command_surface_revision = Some(client.shell_projection_revision);
            let deferred_worktree = matches!(
                &request.method,
                api::schema::Method::WorktreeCreate(_) | api::schema::Method::WorktreeRemove(_)
            );
            let deferred_navigation = matches!(
                &request.method,
                api::schema::Method::WorktreeCreate(params) if params.focus
            );
            client.shell_deferred_navigation_request_id =
                deferred_worktree.then(|| api_request_id.clone());
            client.shell_deferred_navigation_response = deferred_navigation.then(Vec::new);
        }
        let foreground_changed = self.promote_client_to_foreground(client_id);
        foreground_changed
            | self.handle_client_shell_api_request(
                client_id,
                api::ApiRequestMessage {
                    request: *request,
                    respond_to,
                    response_write_complete: None,
                    stream_active: None,
                },
            )
    }

    fn handle_file_put_request(
        &mut self,
        client_id: u64,
        boot_id: String,
        request_id: String,
        method: &api::schema::Method,
    ) -> bool {
        let result = match method {
            api::schema::Method::FilePutBegin(params) => self.file_put_begin(client_id, params),
            api::schema::Method::FilePutChunk(params) => self.file_put_chunk(client_id, params),
            api::schema::Method::FilePutCommit(params) => self.file_put_commit(client_id, params),
            api::schema::Method::FilePutAbort(params) => self.file_put_abort(client_id, params),
            _ => Err(crate::server::file_transfer::TransferError::new(
                "invalid_request",
                "this method is not a file transfer method",
            )),
        };
        let message = match result {
            Ok(result) => crate::server::client_commands::success_message_with_result(
                boot_id, request_id, result,
            ),
            Err(error) => crate::server::client_commands::error_message(
                boot_id,
                request_id,
                error.code,
                error.message,
            ),
        };
        self.send_to_client(client_id, message);
        false
    }

    fn file_put_begin(
        &mut self,
        client_id: u64,
        params: &api::schema::FilePutBeginParams,
    ) -> Result<api::schema::ResponseResult, crate::server::file_transfer::TransferError> {
        let home = crate::integration::home_dir().map_err(|err| {
            crate::server::file_transfer::TransferError::from_io("destination_refused", &err)
        })?;
        let (root, allow_subdirectories) = match params.destination {
            api::schema::FilePutDestination::Inbox => {
                (self.file_transfers.config().inbox.clone(), true)
            }
            api::schema::FilePutDestination::PaneCwd => {
                let Some(pane_id) = params.pane_id.as_deref() else {
                    return Err(crate::server::file_transfer::TransferError::new(
                        "destination_refused",
                        "this destination needs a pane id",
                    ));
                };
                let Some(cwd) = self.app.pane_launch_cwd(pane_id) else {
                    return Err(crate::server::file_transfer::TransferError::new(
                        "destination_refused",
                        "this pane has no resolvable working directory",
                    ));
                };
                (cwd, false)
            }
            api::schema::FilePutDestination::HomePath => {
                let typed = params.home_path.as_deref().unwrap_or("");
                let root = crate::server::file_transfer::destination::ensure_home_relative_root(
                    typed, &home,
                )?;
                (root, true)
            }
        };
        let accepted = self.file_transfers.begin(
            client_id,
            &root,
            &home,
            allow_subdirectories,
            crate::server::file_transfer::BeginEntry {
                suggested_name: &params.suggested_name,
                relative_path: params.relative_path.as_deref(),
                kind: params.entry_kind,
                bytes: params.bytes,
                sha256: &params.sha256,
            },
        )?;
        Ok(api::schema::ResponseResult::FilePutBegan {
            transfer_id: accepted.transfer_id,
            chunk_bytes: accepted.chunk_bytes,
            destination_label: accepted.destination_label,
            complete: accepted.complete,
            path: accepted
                .path
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
    }

    fn file_put_chunk(
        &mut self,
        client_id: u64,
        params: &api::schema::FilePutChunkParams,
    ) -> Result<api::schema::ResponseResult, crate::server::file_transfer::TransferError> {
        use base64::Engine as _;

        let data = base64::engine::general_purpose::STANDARD
            .decode(params.data_b64.as_bytes())
            .map_err(|err| {
                crate::server::file_transfer::TransferError::new(
                    "invalid_file_path",
                    format!("chunk payload is not valid base64: {err}"),
                )
            })?;
        let next_offset =
            self.file_transfers
                .chunk(client_id, &params.transfer_id, params.offset, &data)?;
        Ok(api::schema::ResponseResult::FilePutChunkAccepted {
            transfer_id: params.transfer_id.clone(),
            next_offset,
        })
    }

    fn file_put_commit(
        &mut self,
        client_id: u64,
        params: &api::schema::FilePutCommitParams,
    ) -> Result<api::schema::ResponseResult, crate::server::file_transfer::TransferError> {
        let committed = self.file_transfers.commit(client_id, &params.transfer_id)?;
        Ok(api::schema::ResponseResult::FilePutCommitted {
            transfer_id: params.transfer_id.clone(),
            path: committed.path.to_string_lossy().into_owned(),
            bytes: committed.bytes,
        })
    }

    fn file_put_abort(
        &mut self,
        client_id: u64,
        params: &api::schema::FilePutAbortParams,
    ) -> Result<api::schema::ResponseResult, crate::server::file_transfer::TransferError> {
        self.file_transfers.abort(client_id, &params.transfer_id)?;
        Ok(api::schema::ResponseResult::Ok {})
    }
}
