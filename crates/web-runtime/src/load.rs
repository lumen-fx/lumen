//! Reading an app out of the page it was emitted into.
//!
//! Everything an app is travels as data beside the document: a manifest
//! naming the files, the compiled artifact, one image per script, and the
//! signal state the page was rendered from. This module fetches that set and
//! hands back the parts; starting an app out of them is [`crate::boot`].
//!
//! Paths in the manifest are relative to the site root, which the document
//! carries as `data-lm-base`, so a site published under a prefix resolves the
//! same way from any page in it.

use std::error::Error;
use std::fmt;

use js_sys::Uint8Array;
use lumen_html::contract::{
    DATA_LM_BASE, DATA_LM_CONTRACT, DATA_LM_PAGE, DEFAULT_MANIFEST_FILE, LM_CONTRACT_VERSION,
    Manifest, SEED_SCRIPT_ID, Seed,
};
use lumen_ir::artifact::{self, CompiledApp};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Document, Response, Window};

/// What the document says about itself before anything is fetched.
pub struct PageContext {
    /// The document this runtime is booting into.
    pub document: Document,
    /// Site root every path in the manifest hangs off, with a trailing slash.
    pub base: String,
    /// Page key this document was emitted for.
    pub page: String,
}

/// The app the page names, loaded and ready to start.
pub struct LoadedApp {
    /// Markup, stylesheet, fragments and routing, as the compiler wrote them.
    pub artifact: CompiledApp,
    /// Signal state the page was rendered from.
    pub seed: Seed,
    /// One loaded script per manifest entry, in manifest order.
    pub scripts: Vec<LoadedScript>,
}

/// A script image and the engine that runs it.
pub struct LoadedScript {
    /// Engine name, as the manifest gives it.
    pub engine: String,
    /// Where it was fetched from, which is what a load error names.
    pub uri: String,
    /// The image itself: bytecode, or source text as bytes.
    pub bytes: Vec<u8>,
}

/// Why an app could not be loaded into the page.
#[derive(Debug)]
pub enum LoadError {
    /// There is no window and so no document: a worker, not a page.
    NoDocument,
    /// The document is missing the attribute that says what it is.
    MissingAttribute(&'static str),
    /// The document or the manifest was emitted against a contract this
    /// runtime does not implement.
    Contract {
        /// What the document or manifest said.
        found: String,
        /// What this runtime reads.
        expected: u32,
    },
    /// A file the manifest names could not be fetched.
    Fetch {
        /// The URL asked for.
        url: String,
        /// What the browser said, or the status it answered with.
        reason: String,
    },
    /// A fetched file was not what it claimed to be.
    Parse {
        /// The URL it came from.
        url: String,
        /// What the decoder said.
        reason: String,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::NoDocument => f.write_str("no document to boot into"),
            LoadError::MissingAttribute(name) => write!(
                f,
                "the document carries no `{name}`; it was not emitted by `lumenc web`"
            ),
            LoadError::Contract { found, expected } => write!(
                f,
                "this page was emitted against web contract {found}, and this runtime reads {expected}"
            ),
            LoadError::Fetch { url, reason } => write!(f, "could not fetch {url}: {reason}"),
            LoadError::Parse { url, reason } => write!(f, "could not read {url}: {reason}"),
        }
    }
}

impl Error for LoadError {}

/// What a JavaScript exception says, for a message a reader can act on.
fn js_reason(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            error
                .dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| format!("{error:?}"))
}

/// Join a site-relative path onto the base path.
fn url(base: &str, path: &str) -> String {
    format!("{}{}", base, path.trim_start_matches('/'))
}

impl PageContext {
    /// Read what the document says about itself.
    ///
    /// # Errors
    ///
    /// There is no document, it was not emitted by `lumenc web`, or it was
    /// emitted against a contract this runtime does not implement.
    pub fn from_document() -> Result<Self, LoadError> {
        let window: Window = web_sys::window().ok_or(LoadError::NoDocument)?;
        let document = window.document().ok_or(LoadError::NoDocument)?;
        let root = document
            .document_element()
            .ok_or(LoadError::MissingAttribute(DATA_LM_CONTRACT))?;
        let attribute = |name: &'static str| {
            root.get_attribute(name)
                .ok_or(LoadError::MissingAttribute(name))
        };

        let contract = attribute(DATA_LM_CONTRACT)?;
        if contract.parse::<u32>() != Ok(LM_CONTRACT_VERSION) {
            return Err(LoadError::Contract {
                found: contract,
                expected: LM_CONTRACT_VERSION,
            });
        }
        Ok(Self {
            base: attribute(DATA_LM_BASE)?,
            page: attribute(DATA_LM_PAGE)?,
            document,
        })
    }

    /// The URL of the manifest for this site.
    pub fn manifest_url(&self) -> String {
        url(&self.base, DEFAULT_MANIFEST_FILE)
    }

    /// The signal state this page was rendered from.
    ///
    /// A page emitted without a runtime carries no seed block, and a page
    /// whose app has no state carries an empty one; both start from nothing.
    ///
    /// # Errors
    ///
    /// The block is there but is not a seed this runtime reads.
    pub fn seed(&self) -> Result<Seed, LoadError> {
        let Some(block) = self.document.get_element_by_id(SEED_SCRIPT_ID) else {
            return Ok(Seed::new());
        };
        let json = block.text_content().unwrap_or_default();
        let seed: Seed = serde_json::from_str(&json).map_err(|e| LoadError::Parse {
            url: format!("#{SEED_SCRIPT_ID}"),
            reason: e.to_string(),
        })?;
        if seed.contract_version != LM_CONTRACT_VERSION {
            return Err(LoadError::Contract {
                found: seed.contract_version.to_string(),
                expected: LM_CONTRACT_VERSION,
            });
        }
        Ok(seed)
    }

    /// Fetch the manifest, the artifact and every script it names.
    ///
    /// # Errors
    ///
    /// A file could not be fetched, or was not what the manifest said it was.
    pub async fn load(&self, manifest_url: &str) -> Result<(Manifest, LoadedApp), LoadError> {
        let manifest = fetch_text(manifest_url).await?;
        let manifest: Manifest = serde_json::from_str(&manifest).map_err(|e| LoadError::Parse {
            url: manifest_url.to_owned(),
            reason: e.to_string(),
        })?;
        if manifest.contract_version != LM_CONTRACT_VERSION {
            return Err(LoadError::Contract {
                found: manifest.contract_version.to_string(),
                expected: LM_CONTRACT_VERSION,
            });
        }

        let artifact_url = url(&self.base, &manifest.artifact);
        let bytes = fetch_bytes(&artifact_url).await?;
        let artifact = artifact::read_bytes(&bytes).map_err(|e| LoadError::Parse {
            url: artifact_url,
            reason: e.to_string(),
        })?;

        let mut scripts = Vec::with_capacity(manifest.scripts.len());
        for script in &manifest.scripts {
            let uri = url(&self.base, &script.path);
            scripts.push(LoadedScript {
                engine: script.engine.clone(),
                bytes: fetch_bytes(&uri).await?,
                uri,
            });
        }

        let loaded = LoadedApp {
            artifact,
            seed: self.seed()?,
            scripts,
        };
        Ok((manifest, loaded))
    }
}

/// Fetch `url`, or say why it could not be fetched.
async fn fetch(url: &str) -> Result<Response, LoadError> {
    let window = web_sys::window().ok_or(LoadError::NoDocument)?;
    let response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| LoadError::Fetch {
            url: url.to_owned(),
            reason: js_reason(&e),
        })?;
    let response: Response = response.unchecked_into();
    if !response.ok() {
        return Err(LoadError::Fetch {
            url: url.to_owned(),
            reason: format!("the server answered {}", response.status()),
        });
    }
    Ok(response)
}

/// Fetch `url` as text.
async fn fetch_text(url: &str) -> Result<String, LoadError> {
    let response = fetch(url).await?;
    let text = response.text().map_err(|e| LoadError::Fetch {
        url: url.to_owned(),
        reason: js_reason(&e),
    })?;
    JsFuture::from(text)
        .await
        .map(|value| value.as_string().unwrap_or_default())
        .map_err(|e| LoadError::Fetch {
            url: url.to_owned(),
            reason: js_reason(&e),
        })
}

/// Fetch `url` as bytes.
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, LoadError> {
    let response = fetch(url).await?;
    let buffer = response.array_buffer().map_err(|e| LoadError::Fetch {
        url: url.to_owned(),
        reason: js_reason(&e),
    })?;
    let buffer = JsFuture::from(buffer).await.map_err(|e| LoadError::Fetch {
        url: url.to_owned(),
        reason: js_reason(&e),
    })?;
    Ok(Uint8Array::new(&buffer).to_vec())
}
