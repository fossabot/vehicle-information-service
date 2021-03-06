// SPDX-License-Identifier: MIT

#![feature(await_macro, async_await, futures_api)]

use futures::compat::*;
use futures::StreamExt;
use log::debug;
use serde::de::DeserializeOwned;
use serde_json;
use std::convert::Into;
use std::io;
use std::sync::{Arc, Mutex};
use tokio::prelude::{Sink, Stream};
use tokio_tcp::TcpStream;
use vehicle_information_service::api_type::*;
use websocket::{ClientBuilder, OwnedMessage, WebSocketError};

#[derive(Debug)]
pub enum VISClientError {
    WebSocketError(WebSocketError),
    SerdeError(serde_json::Error),
    IoError(io::Error),
    Other,
}

impl From<WebSocketError> for VISClientError {
    fn from(ws_error: WebSocketError) -> Self {
        VISClientError::WebSocketError(ws_error)
    }
}

impl From<serde_json::Error> for VISClientError {
    fn from(json_error: serde_json::Error) -> Self {
        VISClientError::SerdeError(json_error)
    }
}

impl From<io::Error> for VISClientError {
    fn from(io_error: io::Error) -> Self {
        VISClientError::IoError(io_error)
    }
}

pub struct VISClient {
    #[allow(dead_code)]
    server_address: String,
    client: websocket::client::r#async::Client<TcpStream>,
}

impl VISClient {
    pub async fn connect(server_address: String) -> io::Result<Self> {
        let (client, _headers) = await!(ClientBuilder::new(&server_address)
            .unwrap()
            .async_connect_insecure()
            .compat())
        .unwrap();
        debug!("Connected");
        Ok(Self {
            server_address,
            client,
        })
    }

    /// Retrieve vehicle signals.
    pub async fn get<T>(self, path: ActionPath) -> io::Result<T>
    where
        T: DeserializeOwned,
    {
        let request_id = ReqID::default();
        let get = Action::Get { path, request_id };

        let get_msg = serde_json::to_string(&get).expect("Failed to serialize get");

        let (sink, stream) = self.client.split();

        await!(sink.send(OwnedMessage::Text(get_msg)).compat()).expect("Failed to send message");

        let mut get_stream = stream
            .filter_map(|msg| {
                debug!("VIS Message {:#?}", msg);

                if let OwnedMessage::Text(txt) = msg {
                    let response = serde_json::from_str::<ActionSuccessResponse>(&txt)
                        .expect("Failed to deserialize VIS response");
                    if let ActionSuccessResponse::Get {
                        request_id: resp_request_id,
                        value,
                        ..
                    } = response
                    {
                        if request_id != resp_request_id {
                            return None;
                        }

                        return serde_json::from_value(value)
                            .expect("Failed to deserialize GET Value");
                    }
                    None
                } else {
                    None
                }
            })
            .compat();

        let get_response = await!(get_stream.next());
        Ok(get_response.unwrap().unwrap())
    }

    /// Subscribe to the given path's vehicle signals.
    /// This will return a stream containing all incoming values
    pub async fn subscribe_raw(
        self,
        path: ActionPath,
        filters: Option<Filters>,
    ) -> impl Stream<Item = ActionSuccessResponse, Error = VISClientError> {
        let request_id = ReqID::default();
        let subscribe = Action::Subscribe {
            path,
            filters,
            request_id,
        };

        let subscribe_msg =
            serde_json::to_string(&subscribe).expect("Failed to serialize subscribe");

        let (sink, stream) = self.client.split();

        await!(sink.send(OwnedMessage::Text(subscribe_msg)).compat())
            .expect("Failed to send message");
        stream
            .filter_map(|msg| {
                debug!("VIS Message {:#?}", msg);
                if let OwnedMessage::Text(txt) = msg {
                    Some(
                        serde_json::from_str::<ActionSuccessResponse>(&txt)
                            .expect("Failed to deserialize VIS response"),
                    )
                } else {
                    None
                }
            })
            .map_err(Into::into)
    }

    /// Subscribe to the given path's vehicle signals.
    pub async fn subscribe<T>(
        self,
        path: ActionPath,
        filters: Option<Filters>,
    ) -> impl Stream<Item = (SubscriptionID, T), Error = VISClientError>
    where
        T: DeserializeOwned,
    {
        let (sink, stream) = self.client.split();

        let request_id = ReqID::default();
        let subscribe = Action::Subscribe {
            path,
            filters,
            request_id,
        };

        let subscribe_msg = serde_json::to_string(&subscribe).expect("Failed to serialize message");

        await!(sink.send(OwnedMessage::Text(subscribe_msg)).compat())
            .expect("Failed to send message");

        let subscription_id: Arc<Mutex<Option<SubscriptionID>>> = Default::default();

        stream
            .filter_map(move |msg| {
                debug!("VIS Message {:#?}", msg);

                if let OwnedMessage::Text(txt) = msg {
                    let action_success = serde_json::from_str::<ActionSuccessResponse>(&txt)
                        .expect("Failed to deserialize VIS response");

                    match action_success {
                        ActionSuccessResponse::Subscribe {
                            subscription_id: resp_subscription_id,
                            request_id: resp_request_id,
                            ..
                        } => {
                            // Make sure this is actually the response to our subscription request
                            if resp_request_id != request_id {
                                return None;
                            }
                            // Store subscription_id to make sure the stream only returns values based on this subscription
                            *subscription_id.lock().unwrap() = Some(resp_subscription_id);
                            return None;
                        }
                        ActionSuccessResponse::Subscription {
                            subscription_id: resp_subscription_id,
                            value,
                            ..
                        } => {
                            if *subscription_id.lock().unwrap() != Some(resp_subscription_id) {
                                return None;
                            }

                            let stream_value = serde_json::from_value::<T>(value)
                                .expect("Failed to deserialize subscription value");
                            return Some((resp_subscription_id, stream_value));
                        }
                        _ => (),
                    }
                }
                None
            })
            .map_err(Into::into)
    }

    /// Subscribe to the given path's vehicle signals.
    pub async fn unsubscribe_all<T>(self) -> impl Stream<Item = (), Error = VISClientError>
    where
        T: DeserializeOwned,
    {
        let request_id = ReqID::default();
        let unsubscribe_all = Action::UnsubscribeAll { request_id };

        let unsubscribe_all_msg =
            serde_json::to_string(&unsubscribe_all).expect("Failed to serialize message");

        let (sink, stream) = self.client.split();

        await!(sink.send(OwnedMessage::Text(unsubscribe_all_msg)).compat())
            .expect("Failed to send message");

        stream
            .filter_map(move |msg| {
                debug!("VIS Message {:#?}", msg);

                if let OwnedMessage::Text(txt) = msg {
                    let action_success = serde_json::from_str::<ActionSuccessResponse>(&txt)
                        .expect("Failed to deserialize VIS response");
                    if let ActionSuccessResponse::UnsubscribeAll {
                        request_id: resp_request_id,
                        ..
                    } = action_success
                    {
                        if resp_request_id != request_id {
                            return None;
                        }

                        return Some(());
                    }
                    None
                } else {
                    None
                }
            })
            .map_err(Into::into)
    }
}
