use bevy::app::Events;
use bevy::asset::{AssetLoader, AssetServerError, BoxedFuture, LoadContext, LoadedAsset};
use bevy::prelude::*;
use bevy::reflect::TypeUuid;
use fluent::{bundle::FluentBundle, FluentArgs, FluentResource, FluentValue};
use fluent_syntax::ast::Pattern;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use unic_langid::{LanguageIdentifier, LanguageIdentifierError};

#[derive(Debug, TypeUuid)]
#[uuid = "4df317fb-0581-44f6-8b5f-7cbf12ddc460"]
pub struct I18nLanguageFile(FluentResource);

#[derive(Default)]
pub struct I18nLanguageFileAssetLoader;

impl AssetLoader for I18nLanguageFileAssetLoader {
	fn load<'a>(
		&'a self,
		bytes: &'a [u8],
		load_context: &'a mut LoadContext,
	) -> BoxedFuture<'a, anyhow::Result<()>> {
		Box::pin(async move {
			let data = String::from_utf8(Vec::from(bytes))?;
			let res = match FluentResource::try_new(data) {
				Ok(res) => res,
				Err((res, errors)) => {
					for error in errors {
						error!(
							"`FluentResource` parse error from `{:?}`: {:?}",
							load_context.path(),
							error
						);
					}
					res
				}
			};
			load_context.set_default_asset(LoadedAsset::new(I18nLanguageFile(res)));
			Ok(())
		})
	}

	fn extensions(&self) -> &[&str] {
		&["ftl"]
	}
}

type Bundle = FluentBundle<FluentResource, intl_memoizer::concurrent::IntlLangMemoizer>;

pub struct I18n {
	root_path: PathBuf,
	bundles: Vec<(Vec<(bool, Handle<I18nLanguageFile>)>, Bundle)>,
}

pub struct I18nLanguageChangedEvent;
pub struct I18nChangeLanguageTo(pub Vec<LanguageIdentifier>);

pub struct I18nPlugin {
	root_path: PathBuf,
	languages: Vec<LanguageIdentifier>,
}

impl I18nPlugin {
	pub fn new(root_path: PathBuf, languages: Vec<LanguageIdentifier>) -> Self {
		Self {
			root_path,
			languages,
		}
	}
}

impl Plugin for I18nPlugin {
	fn build(&self, app: &mut AppBuilder) {
		app.add_event::<I18nLanguageChangedEvent>()
			.add_event::<I18nChangeLanguageTo>()
			.add_asset::<I18nLanguageFile>()
			.init_asset_loader::<I18nLanguageFileAssetLoader>();

		let mut lang = I18n::new(self.root_path.clone());

		{
			let world = app.app.world.cell();
			let asset_server = world
				.get_resource::<AssetServer>()
				.expect("`AssetServer` must be registered as a resource before `I18n` is built");
			let assets = world.get_resource::<Assets<I18nLanguageFile>>().expect("just registered asset is apparently missing its assets container for `I18nLanguageFile`");
			let mut changed_events= world.get_resource_mut::<Events<I18nLanguageChangedEvent>>().expect("just registered event is apparently missing its resource for I18nLanguageChangedEvent");
			lang.change_language_to(
				&self.languages,
				&*asset_server,
				&*assets,
				&mut changed_events,
			)
			.expect("initial language selected does not exist");
		}

		app.insert_resource(lang)
			.add_system(language_asset_loaded.system())
			.add_system(change_language.system());
	}
}

fn language_asset_loaded(
	mut ev_asset: EventReader<AssetEvent<I18nLanguageFile>>,
	assets: Res<Assets<I18nLanguageFile>>,
	mut changed: EventWriter<I18nLanguageChangedEvent>,
	mut lang: ResMut<I18n>,
) {
	for ev in ev_asset.iter() {
		info!("language asset state changed: {:?}", &ev);
		match ev {
			AssetEvent::Created { handle } => {
				lang.update_bundle_for_matching_handle_asset(&handle, &*assets, &mut changed)
			}
			AssetEvent::Modified { handle } => {
				lang.update_bundle_for_matching_handle_asset(&handle, &*assets, &mut changed)
			}
			AssetEvent::Removed { handle } => {
				let _ = lang.remove_tracked_handle(&handle, &*assets, &mut changed);
			}
		}
	}
}

// #[derive(thiserror::Error, Debug)]
// pub enum I18nError {
// 	#[error("Resource parse error: {0:?}")]
// 	ResourceParseError(Vec<ParserError>),
// }

impl I18n {
	pub fn new(root_path: PathBuf) -> Self {
		Self {
			root_path,
			bundles: vec![],
		}
	}

	pub fn remaining_to_load(&self) -> usize {
		self.bundles
			.iter()
			.map(|(handles, _bundle)| handles)
			.flatten()
			.filter(|(loaded, _handle)| !*loaded)
			.count()
	}

	pub fn is_fully_loaded(&self) -> bool {
		self.bundles
			.iter()
			.map(|(handles, _bundle)| handles)
			.flatten()
			.all(|(loaded, _handle)| *loaded)
	}

	fn update_bundle_for_matching_handle_asset(
		&mut self,
		handle: &Handle<I18nLanguageFile>,
		assets: &Assets<I18nLanguageFile>,
		changed: &mut EventWriter<I18nLanguageChangedEvent>,
	) {
		if let Some(asset) = assets.get(handle) {
			for (handles, bundle) in self.bundles.iter_mut() {
				if let Some((loaded, _handle)) = handles.iter_mut().find(|t| t.1 == *handle) {
					// Yes this re-parse is bad but FluentResource doesn't implement `Clone`.
					// Ignoring errors since they were reported in the Asset itself earlier.
					let res = FluentResource::try_new(asset.0.source().to_owned())
						.unwrap_or_else(|(res, _errors)| res);
					if *loaded {
						bundle.add_resource_overriding(res);
					} else {
						if let Err(errors) = bundle.add_resource(res) {
							for error in errors {
								error!("duplicate message already exists in bundle: {:?}", error);
							}
						}
					}
					*loaded = true;
					break;
				}
			}
			if self.is_fully_loaded() {
				changed.send(I18nLanguageChangedEvent);
			}
		}
	}

	fn remove_tracked_handle(
		&mut self,
		handle: &Handle<I18nLanguageFile>,
		assets: &Assets<I18nLanguageFile>,
		changed: &mut EventWriter<I18nLanguageChangedEvent>,
	) -> Option<usize> {
		for (idx, (handles, _bundle)) in self.bundles.iter_mut().enumerate() {
			if let Some(i) = handles
				.iter()
				.map(|(_enabled, handle)| handle)
				.enumerate()
				.find_map(|(i, h)| if h == handle { Some(i) } else { None })
			{
				handles.swap_remove(i);
				self.reload_bundle_assets_at(assets, changed, idx);
				return Some(idx);
			}
		}
		None
	}

	fn reload_bundle_assets_at(
		&mut self,
		assets: &Assets<I18nLanguageFile>,
		changed: &mut EventWriter<I18nLanguageChangedEvent>,
		idx: usize,
	) {
		let (handles, bundle) = &mut self.bundles[idx];
		*bundle = Bundle::new_concurrent(bundle.locales.clone());
		bundle.set_use_isolating(false);
		for (_enabled, handle) in handles.iter() {
			if let Some(asset) = assets.get(handle) {
				// Yes this re-parse is bad but FluentResource doesn't implement `Clone`.
				// Ignoring errors since they were reported in the Asset itself earlier.
				let res = FluentResource::try_new(asset.0.source().to_owned())
					.unwrap_or_else(|(res, _errors)| res);
				bundle.add_resource_overriding(res);
			}
		}
		if self.is_fully_loaded() {
			changed.send(I18nLanguageChangedEvent);
		}
		todo!()
	}

	fn init_bundle_from_language(
		root_path: &Path,
		asset_server: &AssetServer,
		assets: &Assets<I18nLanguageFile>,
		language: &LanguageIdentifier,
	) -> Result<(Vec<(bool, Handle<I18nLanguageFile>)>, Bundle), AssetServerError> {
		let mut bundle = Bundle::new_concurrent(vec![language.clone()]);
		bundle.set_use_isolating(false);
		let mut path = root_path.to_owned();
		path.push(language.to_string());
		let handles = asset_server
			.load_folder(&path)?
			.iter()
			.map(|h| {
				let handle = h.clone().typed::<I18nLanguageFile>();
				// Pre-emptively try to load the asset if already available
				let loaded = if let Some(asset) = assets.get(&handle) {
					let res = FluentResource::try_new(asset.0.source().to_owned())
						.unwrap_or_else(|(res, _errors)| res);
					if let Err(errors) = bundle.add_resource(res) {
						for error in errors {
							error!("duplicate message already exists in bundle: {:?}", error);
						}
					}
					true
				} else {
					false
				};
				(loaded, handle)
			})
			.collect();
		Ok((handles, bundle))
	}

	pub fn change_language_to(
		&mut self,
		languages: &Vec<LanguageIdentifier>,
		asset_server: &AssetServer,
		assets: &Assets<I18nLanguageFile>,
		changed: &mut Events<I18nLanguageChangedEvent>,
	) -> Result<(), AssetServerError> {
		if self.bundles.len() != languages.len()
			|| self
				.bundles
				.iter()
				.map(|(_handles, bundle)| bundle.locales.first())
				.zip(languages.iter())
				.any(|(a, b)| a != Some(b))
		{
			info!("changing language to: {:?}", languages);
			self.bundles = languages
				.iter()
				.map(|l| Self::init_bundle_from_language(&self.root_path, asset_server, assets, l))
				.collect::<Result<_, _>>()?;
			if self.is_fully_loaded() {
				changed.send(I18nLanguageChangedEvent);
			}
			Ok(())
		} else {
			info!(
				"language change requested but it is already that language: {:?}",
				languages
			);
			Ok(())
		}
	}

	// pub fn add_language_function<F>(
	// 	&mut self,
	// 	language: LanguageIdentifier,
	// 	id: &str,
	// 	func: F,
	// ) -> Result<&mut Self, FluentError>
	// where
	// 	F: for<'a> Fn(&[FluentValue<'a>], &FluentArgs<'_>) -> FluentValue<'a>
	// 		+ Sync
	// 		+ Send
	// 		+ 'static,
	// {
	// 	for bundle in &mut self.bundles {
	// 		if bundle.locales.contains(&language) {
	// 			bundle.add_function(id, func)?;
	// 			break;
	// 		}
	// 	}
	// 	Ok(self)
	// }

	fn format_string<'s>(
		bundle: &'s Bundle,
		id: &'s str,
		pattern: &'s Pattern<&str>,
		args: Option<&'s FluentArgs>,
	) -> Cow<'s, str> {
		let mut errs = vec![];
		let str = bundle.format_pattern(pattern, args, &mut errs);
		if !errs.is_empty() {
			error!(
				"Message Format Errors of message ID `{}` with pattern `{:?}` and args `{:?}`: {:?}",
				id, pattern, args, errs
			);
		}
		str
	}

	pub fn get<'i, 's: 'i>(&'s self, id: &'i str) -> Cow<'i, str> {
		for (_handles, bundle) in self.bundles.iter() {
			if let Some(msg) = bundle.get_message(id) {
				if let Some(value) = msg.value() {
					return Self::format_string(&bundle, id, value, None);
				}
			}
		}

		if let Some(locale) = self
			.bundles
			.first()
			.map(|(_handles, bundle)| bundle.locales.first())
			.flatten()
		{
			error!(
				"I18n Message ID `{}` not found for language `{}`",
				id, locale
			);
		}
		Cow::Owned(format!("##~{}~##", id))
	}

	pub fn get_with_args<'i, 's: 'i>(&'s self, id: &'i str, args: &'i FluentArgs) -> Cow<'i, str> {
		for (_handles, bundle) in self.bundles.iter() {
			if let Some(msg) = bundle.get_message(id) {
				if let Some(value) = msg.value() {
					return Self::format_string(&bundle, id, value, Some(args));
				}
			}
		}

		if let Some(locale) = self
			.bundles
			.first()
			.map(|(_handles, bundle)| bundle.locales.first())
			.flatten()
		{
			error!(
				"I18n Message ID `{}` not found for language `{}`",
				id, locale
			);
		}
		Cow::Owned(format!("##~{}~##", id))
	}

	pub fn get_with_args_list<'i, 's: 'i, K, V, I>(&'s self, id: &'i str, args: I) -> Cow<'i, str>
	where
		K: Into<Cow<'i, str>>,
		V: Into<FluentValue<'i>>,
		I: IntoIterator<Item = (K, V)>,
	{
		for (_handles, bundle) in self.bundles.iter() {
			if let Some(msg) = bundle.get_message(id) {
				if let Some(value) = msg.value() {
					let args: FluentArgs<'i> = args.into_iter().collect();
					// We can't point to things in this stackframe (since it's about to pop), so have
					// to make this owned.
					return Cow::Owned(
						Self::format_string(&bundle, id, value, Some(&args)).into_owned(),
					);
				}
			}
		}

		if let Some(locale) = self
			.bundles
			.first()
			.map(|(_handles, bundle)| bundle.locales.first())
			.flatten()
		{
			error!(
				"I18n Message ID `{}` not found for language `{}`",
				id, locale
			);
		}
		Cow::Owned(format!("##~{}~##", id))
	}

	pub fn get_attr<'i, 's: 'i>(&'s self, id: &'i str, attr: &'i str) -> Cow<'i, str> {
		for (_handles, bundle) in self.bundles.iter() {
			if let Some(msg) = bundle.get_message(id) {
				if let Some(value) = msg.get_attribute(attr) {
					return Self::format_string(&bundle, id, value.value(), None);
				}
			}
		}

		if let Some(locale) = self
			.bundles
			.first()
			.map(|(_handles, bundle)| bundle.locales.first())
			.flatten()
		{
			error!(
				"I18n Message ID `{}` and attr `{}` not found for language `{}`",
				id, attr, locale
			);
		}
		Cow::Owned(format!("##~{}~@@~{}~##", id, attr))
	}

	pub fn get_attr_with_args<'i, 's: 'i>(
		&'s self,
		id: &'i str,
		attr: &'i str,
		args: &'i FluentArgs,
	) -> Cow<'i, str> {
		for (_handles, bundle) in self.bundles.iter() {
			if let Some(msg) = bundle.get_message(id) {
				if let Some(value) = msg.get_attribute(attr) {
					return Self::format_string(&bundle, id, value.value(), Some(args));
				}
			}
		}

		if let Some(locale) = self
			.bundles
			.first()
			.map(|(_handles, bundle)| bundle.locales.first())
			.flatten()
		{
			error!(
				"I18n Message ID `{}` and attr `{}` not found for language `{}`",
				id, attr, locale
			);
		}
		Cow::Owned(format!("##~{}~@@~{}~##", id, attr))
	}

	pub fn get_attr_with_args_list<'i, 's: 'i, K, V, I>(
		&'s self,
		id: &'i str,
		attr: &'i str,
		args: I,
	) -> Cow<'i, str>
	where
		K: Into<Cow<'i, str>>,
		V: Into<FluentValue<'i>>,
		I: IntoIterator<Item = (K, V)>,
	{
		for (_handles, bundle) in self.bundles.iter() {
			if let Some(msg) = bundle.get_message(id) {
				if let Some(value) = msg.get_attribute(attr) {
					let args: FluentArgs<'i> = args.into_iter().collect();
					// We can't point to things in the stackframe (since it's about to pop), so have
					// to make this owned.
					return Cow::Owned(
						Self::format_string(&bundle, id, value.value(), Some(&args)).into_owned(),
					);
				}
			}
		}

		if let Some(locale) = self
			.bundles
			.first()
			.map(|(_handles, bundle)| bundle.locales.first())
			.flatten()
		{
			error!(
				"I18n Message ID `{}` and attr `{}` not found for language `{}`",
				id, attr, locale
			);
		}
		Cow::Owned(format!("##~{}~@@~{}~##", id, attr))
	}

	pub fn get_current_language(&self) -> LanguageIdentifier {
		self.bundles
			.first()
			.map(|(_handles, b)| b.locales.first().cloned().unwrap_or_else(Default::default))
			.unwrap_or_else(Default::default)
	}
}

fn change_language(
	mut change: EventReader<I18nChangeLanguageTo>,
	mut lang: ResMut<I18n>,
	asset_server: Res<AssetServer>,
	assets: Res<Assets<I18nLanguageFile>>,
	mut changed_events: ResMut<Events<I18nLanguageChangedEvent>>,
) {
	if let Some(I18nChangeLanguageTo(languages)) = change.iter().last() {
		match lang.change_language_to(languages, &asset_server, &assets, &mut *changed_events) {
			Ok(()) => {}
			Err(e) => {
				error!("failed changing language with error: `{:?}", e);
			}
		}
	}
}

pub fn scan_languages_on_fs() -> Result<Vec<LanguageIdentifier>, std::io::Error> {
	// TODO:  Move this to bevy's AssetIO once find a way to expose it...
	// TODO:  Maybe load the AssetServer ourself and hold on to the AssetIO as well...
	let mut ret = Vec::with_capacity(10);
	for path in std::fs::read_dir("./assets/lang")?.flatten() {
		if let Ok(file_type) = path.file_type() {
			if file_type.is_dir() {
				if let Ok(lang) = path
					.path()
					.iter()
					.last()
					.and_then(|l| l.to_str())
					.ok_or(LanguageIdentifierError::Unknown)
					.and_then(|l| l.parse::<LanguageIdentifier>())
				{
					ret.push(lang);
				}
			}
		}
	}
	Ok(ret)
}

#[derive(Debug)]
pub struct MsgKey<'a, 'b, Args> {
	key: Cow<'a, str>,
	attr: Option<Cow<'b, str>>,
	args: Args,
}

pub type MsgKey0<'a, 'b> = MsgKey<'a, 'b, ()>;
pub type MsgKeyA<'a, 'b, 'z> = MsgKey<'a, 'b, FluentArgs<'z>>;

impl<'a> MsgKey<'a, 'static, ()> {
	pub const fn new(key: &'a str) -> MsgKey0<'a, 'static> {
		Self {
			key: Cow::Borrowed(key),
			attr: None,
			args: (),
		}
	}

	pub fn with_attr<'z>(self, attr: impl Into<Cow<'z, str>>) -> MsgKey0<'a, 'z> {
		MsgKey {
			attr: Some(attr.into()),
			..self
		}
	}
}

impl<'a, 'b> MsgKey<'a, 'b, ()> {
	pub const fn new_attr(key: &'a str, attr: &'b str) -> Self {
		MsgKey {
			key: Cow::Borrowed(key),
			attr: Some(Cow::Borrowed(attr)),
			args: (),
		}
	}

	pub fn with_args<'z>(&self, args: FluentArgs<'z>) -> MsgKeyA<'a, 'b, 'z> {
		MsgKey {
			key: self.key.clone(),
			attr: self.attr.clone(),
			args,
		}
	}

	pub fn with_args_iter<'z, K, V, I>(&self, args: I) -> MsgKeyA<'a, 'b, 'z>
	where
		K: Into<Cow<'z, str>>,
		V: Into<FluentValue<'z>>,
		I: IntoIterator<Item = (K, V)>,
	{
		let args: FluentArgs<'z> = args.into_iter().collect();
		MsgKey {
			key: self.key.clone(),
			attr: self.attr.clone(),
			args,
		}
	}

	pub fn translate<'i, 's: 'i>(&'i self, i18n: &'s I18n) -> Cow<'i, str> {
		let key = self.key.as_ref();
		if let Some(attr) = self.attr.as_ref().map(AsRef::as_ref) {
			i18n.get_attr(key, attr)
		} else {
			i18n.get(key)
		}
	}
}

impl<'a, 'b, 'z> MsgKey<'a, 'b, FluentArgs<'z>> {
	pub fn translate<'i, 's: 'i>(&'i self, i18n: &'s I18n) -> Cow<'i, str> {
		let key = self.key.as_ref();
		if let Some(attr) = self.attr.as_ref().map(AsRef::as_ref) {
			i18n.get_attr_with_args(key, attr, &self.args)
		} else {
			i18n.get_with_args(key, &self.args)
		}
	}
}

pub struct MsgCache {
	msg_key: MsgKey<'static, 'static, ()>,
	msg: String,
}

impl MsgCache {
	pub fn new(msg_key: MsgKey<'static, 'static, ()>) -> Self {
		let msg = if let Some(attr) = &msg_key.attr {
			format!("##~!!~{}~!{}!~!!~##", msg_key.key, attr)
		} else {
			format!("##~!!~{}~!!~##", msg_key.key)
		};
		Self { msg_key, msg }
	}

	pub fn no_attr(&mut self) -> &mut Self {
		self.msg_key.attr = None;
		self
	}

	pub fn attr(&mut self, attr: &'static str) -> &mut Self {
		self.msg_key.attr = Some(Cow::Borrowed(attr));
		self
	}

	pub fn update(&mut self, i18n: &I18n) {
		self.msg = self.msg_key.translate(i18n).into_owned();
	}

	pub fn update_args(&mut self, i18n: &I18n, args: FluentArgs) {
		self.msg = self.msg_key.with_args(args).translate(i18n).into_owned();
	}

	pub fn update_args_iter<'z, K, V, I>(&mut self, i18n: &I18n, args: I)
	where
		K: Into<Cow<'z, str>>,
		V: Into<FluentValue<'z>>,
		I: IntoIterator<Item = (K, V)>,
	{
		self.msg = self
			.msg_key
			.with_args_iter(args)
			.translate(i18n)
			.into_owned();
	}

	pub fn as_str(&self) -> &str {
		&self.msg
	}
}

#[cfg(test)]
mod test {
	use crate::universal::i18n::{Bundle, MsgKey};
	use crate::universal::I18n;
	use fluent::{FluentArgs, FluentResource};

	fn test_i18n() -> I18n {
		let mut i18n = I18n {
			root_path: Default::default(),
			bundles: vec![],
		};
		let mut bundle = Bundle::new_concurrent(vec!["en-US".parse().unwrap()]);
		bundle.set_use_isolating(false);
		bundle
			.add_resource(
				FluentResource::try_new(
					r#"
title = Test Title
  .an_attr = Title Attr
no_default =
  .with_attr = No Default With Attr
with_args = String arg is { $str_arg } and number arg is { $num_arg }
  .just_str = String arg is {$str_arg}
"#
					.to_string(),
				)
				.unwrap(),
			)
			.unwrap();
		i18n.bundles.push((vec![], bundle));
		i18n
	}

	fn args() -> FluentArgs<'static> {
		let mut args = FluentArgs::with_capacity(2);
		args.set("str_arg", "stringy");
		args.set("num_arg", 42);
		args
	}

	#[test]
	fn translate() {
		let i18n = test_i18n();
		assert_eq!(i18n.get("title"), "Test Title");
		assert_eq!(i18n.get_attr("title", "an_attr"), "Title Attr");
		assert_eq!(
			i18n.get_with_args("with_args", &args()),
			"String arg is stringy and number arg is 42"
		);
		assert_eq!(MsgKey::new("title").translate(&i18n), "Test Title");
		assert_eq!(
			MsgKey::new("title").with_attr("an_attr").translate(&i18n),
			"Title Attr"
		);
		assert_eq!(
			MsgKey::new("with_args")
				.with_attr("just_str")
				.with_args(args())
				.translate(&i18n),
			"String arg is stringy"
		);
		assert_eq!(
			MsgKey::new("with_args")
				.with_attr("just_str")
				// TODO:  remove the `vec!` part when Rust 1.53 lands as then slices will implement IntoIterator
				.with_args_iter(vec![("str_arg", "another")])
				.translate(&i18n),
			"String arg is another"
		);
	}
}
