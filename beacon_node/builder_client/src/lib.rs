use eth2::types::builder_bid::SignedBuilderBid;
use eth2::types::{
    EthSpec, ExecutionBlockHash, ForkVersionedResponse, PublicKeyBytes,
    SignedValidatorRegistrationData, Slot,
};
use eth2::types::{FullPayloadContents, SignedBlindedBeaconBlock};
pub use eth2::Error;
use eth2::{ok_or_error, StatusCode, CONSENSUS_VERSION_HEADER};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{IntoUrl, Response};
use sensitive_url::SensitiveUrl;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_MILLIS: u64 = 15000;

/// This timeout is in accordance with v0.2.0 of the [builder specs](https://github.com/flashbots/mev-boost/pull/20).
pub const DEFAULT_GET_HEADER_TIMEOUT_MILLIS: u64 = 1000;

/// Default user agent for HTTP requests.
pub const DEFAULT_USER_AGENT: &str = lighthouse_version::VERSION;

#[derive(Clone)]
pub struct Timeouts {
    get_header: Duration,
    post_validators: Duration,
    post_blinded_blocks: Duration,
    get_builder_status: Duration,
}

impl Timeouts {
    fn new(get_header_timeout: Option<Duration>) -> Self {
        let get_header =
            get_header_timeout.unwrap_or(Duration::from_millis(DEFAULT_GET_HEADER_TIMEOUT_MILLIS));

        Self {
            get_header,
            post_validators: Duration::from_millis(DEFAULT_TIMEOUT_MILLIS),
            post_blinded_blocks: Duration::from_millis(DEFAULT_TIMEOUT_MILLIS),
            get_builder_status: Duration::from_millis(DEFAULT_TIMEOUT_MILLIS),
        }
    }
}

#[derive(Clone)]
pub struct BuilderHttpClient {
    client: reqwest::Client,
    server: SensitiveUrl,
    timeouts: Timeouts,
    user_agent: String,
}

impl BuilderHttpClient {
    pub fn new(
        server: SensitiveUrl,
        user_agent: Option<String>,
        builder_header_timeout: Option<Duration>,
    ) -> Result<Self, Error> {
        let user_agent = user_agent.unwrap_or(DEFAULT_USER_AGENT.to_string());
        let client = reqwest::Client::builder().user_agent(&user_agent).build()?;
        Ok(Self {
            client,
            server,
            timeouts: Timeouts::new(builder_header_timeout),
            user_agent,
        })
    }

    pub fn get_user_agent(&self) -> &str {
        &self.user_agent
    }

    async fn get_with_timeout<T: DeserializeOwned, U: IntoUrl>(
        &self,
        url: U,
        timeout: Duration,
    ) -> Result<T, Error> {
        self.get_response_with_timeout(url, Some(timeout))
            .await?
            .json()
            .await
            .map_err(Into::into)
    }

    /// Perform a HTTP GET request, returning the `Response` for further processing.
    async fn get_response_with_timeout<U: IntoUrl>(
        &self,
        url: U,
        timeout: Option<Duration>,
    ) -> Result<Response, Error> {
        let mut builder = self.client.get(url);
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        let response = builder.send().await.map_err(Error::from)?;
        ok_or_error(response).await
    }

    /// Generic POST function supporting arbitrary responses and timeouts.
    async fn post_generic<T: Serialize, U: IntoUrl>(
        &self,
        url: U,
        body: &T,
        timeout: Option<Duration>,
    ) -> Result<Response, Error> {
        let mut builder = self.client.post(url);
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        let response = builder.json(body).send().await?;
        ok_or_error(response).await
    }

    async fn post_with_raw_response<T: Serialize, U: IntoUrl>(
        &self,
        url: U,
        body: &T,
        headers: HeaderMap,
        timeout: Option<Duration>,
    ) -> Result<Response, Error> {
        let mut builder = self.client.post(url);
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }

        let response = builder
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(Error::from)?;
        ok_or_error(response).await
    }

    /// `POST /eth/v1/builder/validators`
    pub async fn post_builder_validators(
        &self,
        validator: &[SignedValidatorRegistrationData],
    ) -> Result<(), Error> {
        let mut path = self.server.full.clone();

        path.path_segments_mut()
            .map_err(|()| Error::InvalidUrl(self.server.clone()))?
            .push("eth")
            .push("v1")
            .push("builder")
            .push("validators");

        self.post_generic(path, &validator, Some(self.timeouts.post_validators))
            .await?;
        Ok(())
    }

    /// `POST /eth/v1/builder/blinded_blocks`
    pub async fn post_builder_blinded_blocks<E: EthSpec>(
        &self,
        blinded_block: &SignedBlindedBeaconBlock<E>,
    ) -> Result<ForkVersionedResponse<FullPayloadContents<E>>, Error> {
        let mut path = self.server.full.clone();

        path.path_segments_mut()
            .map_err(|()| Error::InvalidUrl(self.server.clone()))?
            .push("eth")
            .push("v1")
            .push("builder")
            .push("blinded_blocks");

        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(&blinded_block.fork_name_unchecked().to_string()) {
            headers.insert(CONSENSUS_VERSION_HEADER, value);
        }

        Ok(self
            .post_with_raw_response(
                path,
                &blinded_block,
                headers,
                Some(self.timeouts.post_blinded_blocks),
            )
            .await?
            .json()
            .await?)
    }

    /// `GET /eth/v1/builder/header`
    pub async fn get_builder_header<E: EthSpec>(
        &self,
        slot: Slot,
        parent_hash: ExecutionBlockHash,
        pubkey: &PublicKeyBytes,
    ) -> Result<Option<ForkVersionedResponse<SignedBuilderBid<E>>>, Error> {
        let mut path = self.server.full.clone();

        path.path_segments_mut()
            .map_err(|()| Error::InvalidUrl(self.server.clone()))?
            .push("eth")
            .push("v1")
            .push("builder")
            .push("header")
            .push(slot.to_string().as_str())
            .push(format!("{parent_hash:?}").as_str())
            .push(pubkey.as_hex_string().as_str());

        let resp = self.get_with_timeout(path, self.timeouts.get_header).await;

        if matches!(resp, Err(Error::StatusCode(StatusCode::NO_CONTENT))) {
            Ok(None)
        } else {
            resp.map(Some)
        }
    }

    /// `GET /eth/v1/builder/status`
    pub async fn get_builder_status<E: EthSpec>(&self) -> Result<(), Error> {
        let mut path = self.server.full.clone();

        path.path_segments_mut()
            .map_err(|()| Error::InvalidUrl(self.server.clone()))?
            .push("eth")
            .push("v1")
            .push("builder")
            .push("status");

        self.get_with_timeout(path, self.timeouts.get_builder_status)
            .await
    }
}
