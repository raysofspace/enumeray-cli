//! Typed bounded collection metadata; bulk bytes use the existing artifact API.
use crate::{EnumerayClient, Error, resource_path};
use enumeray_contracts::collections::*;
use reqwest::Method;
use serde_json::{Value, json};
use uuid::Uuid;
impl EnumerayClient {
    pub fn collection(&self, id: Uuid) -> Result<CollectionView, Error> {
        self.get(&resource_path(&["collections", &id.to_string()])?)
    }
    pub fn collection_members(&self, id: Uuid, cursor: u32) -> Result<MemberPage, Error> {
        self.get(&format!(
            "{}?cursor={cursor}",
            resource_path(&["collections", &id.to_string(), "members"])?
        ))
    }
    pub fn reserve_collection(
        &self,
        key: &str,
        request: &CollectionRequest,
    ) -> Result<CollectionReceipt, Error> {
        self.request(
            Method::POST,
            &resource_path(&["collections"])?,
            Some(request),
            Some(key),
        )
    }
    pub fn collection_page(
        &self,
        id: Uuid,
        start: u32,
        members: &[MemberSpec],
    ) -> Result<Value, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "collections",
                &id.to_string(),
                "members",
                &start.to_string(),
            ])?,
            Some(members),
            None,
        )
    }
    pub fn freeze_collection(&self, id: Uuid) -> Result<Value, Error> {
        self.request(
            Method::PUT,
            &resource_path(&["collections", &id.to_string(), "freeze"])?,
            Some(&json!({})),
            None,
        )
    }
    pub fn bind_collection_member(
        &self,
        id: Uuid,
        index: u32,
        artifact: Uuid,
    ) -> Result<Value, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "collections",
                &id.to_string(),
                "members",
                &index.to_string(),
                "artifact",
            ])?,
            Some(&json!({"artifact_id":artifact})),
            None,
        )
    }
    pub fn complete_collection(&self, id: Uuid) -> Result<CollectionView, Error> {
        self.request(
            Method::PUT,
            &resource_path(&["collections", &id.to_string(), "completion"])?,
            Some(&json!({})),
            None,
        )
    }
}
