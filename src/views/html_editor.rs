use crate::i18n::gettext;
// SPDX-License-Identifier: GPL-3.0-or-later
//
// HtmlEditor — WebKitGTK 6.0 based WYSIWYG HTML editor for mail composition.
//
// Features:
// - Contenteditable HTML editor canvas
// - Formatting toolbar (Bold, Italic, Underline, Lists, Blockquote, Links)
// - Sandboxed DOM manipulation via JavaScript

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use webkit6::prelude::*;

use crate::services::settings_service::AppSettings;
use crate::views::accessibility::set_accessible_label;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default, Clone)]
    pub struct CallbackList(pub Vec<std::rc::Rc<dyn Fn(Vec<std::path::PathBuf>) + 'static>>);

    impl std::fmt::Debug for CallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "CallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct DragMotionCallbackList(pub Vec<std::rc::Rc<dyn Fn(f64, f64) + 'static>>);

    impl std::fmt::Debug for DragMotionCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "DragMotionCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct DragLeaveCallbackList(pub Vec<std::rc::Rc<dyn Fn() + 'static>>);

    impl std::fmt::Debug for DragLeaveCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "DragLeaveCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct ImageConvertCallbackList(
        pub Vec<std::rc::Rc<dyn Fn(crate::services::draft_store_service::DraftAttachment) + 'static>>,
    );

    impl std::fmt::Debug for ImageConvertCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "ImageConvertCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct DragInternalCallbackList(
        pub Vec<std::rc::Rc<dyn Fn(bool) + 'static>>,
    );

    impl std::fmt::Debug for DragInternalCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "DragInternalCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Debug, Default)]
    pub struct HtmlEditor {
        pub web_view: RefCell<Option<webkit6::WebView>>,
        pub web_context: RefCell<Option<webkit6::WebContext>>,
        pub format_toolbar: RefCell<Option<gtk::Box>>,
        pub format_toolbar_revealer: RefCell<Option<gtk::Revealer>>,
        pub plain_text_mode: Cell<bool>,
        pub web_view_ready: Cell<bool>,
        pub pending_scripts: RefCell<Vec<String>>,
        pub spell_checking_enabled: Cell<bool>,
        pub spell_checking_languages: RefCell<Vec<String>>,
        pub spelling_handler_ids: RefCell<Vec<glib::SignalHandlerId>>,
        pub files_dropped_callbacks: RefCell<CallbackList>,
        pub drag_motion_callbacks: RefCell<DragMotionCallbackList>,
        pub drag_leave_callbacks: RefCell<DragLeaveCallbackList>,
        pub drag_internal_callbacks: RefCell<DragInternalCallbackList>,
        pub image_convert_callbacks: RefCell<ImageConvertCallbackList>,
        pub active_image_popover: RefCell<Option<gtk::Popover>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HtmlEditor {
        const NAME: &'static str = "MeridianHtmlEditor";
        type Type = super::HtmlEditor;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for HtmlEditor {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(popover) = self.active_image_popover.borrow_mut().take() {
                popover.unparent();
            }
            if let Some(settings) = AppSettings::try_new() {
                for id in self.spelling_handler_ids.borrow_mut().drain(..) {
                    settings.disconnect(id);
                }
            }
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for HtmlEditor {
        fn realize(&self) {
            self.parent_realize();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorContent {
    pub body: String,
    pub plain_text: bool,
}

glib::wrapper! {
    pub struct HtmlEditor(ObjectSubclass<imp::HtmlEditor>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HtmlEditor {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn connect_files_dropped<F>(&self, callback: F)
    where
        F: Fn(Vec<std::path::PathBuf>) + 'static,
    {
        if let Ok(mut cbs) = self.imp().files_dropped_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub fn dispatch_files_dropped(&self, paths: Vec<std::path::PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let cbs: Vec<_> = if let Ok(guard) = self.imp().files_dropped_callbacks.try_borrow() {
            guard.0.clone()
        } else {
            Vec::new()
        };
        for cb in cbs {
            let p = paths.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                cb(p);
            }));
        }
    }

    pub fn connect_drag_motion<F>(&self, callback: F)
    where
        F: Fn(f64, f64) + 'static,
    {
        if let Ok(mut cbs) = self.imp().drag_motion_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub fn connect_drag_leave<F>(&self, callback: F)
    where
        F: Fn() + 'static,
    {
        if let Ok(mut cbs) = self.imp().drag_leave_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub fn connect_drag_internal<F>(&self, callback: F)
    where
        F: Fn(bool) + 'static,
    {
        if let Ok(mut cbs) = self.imp().drag_internal_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub fn is_supported_image(path: &std::path::Path) -> bool {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp")
    }

    /// Loads an image from path and prepares an optimized data URI.
    /// Raster images exceeding 1920px or 1.5 MB are downscaled using gdk-pixbuf
    /// to avoid passing multi-megabyte Base64 strings through WebKit IPC,
    /// preventing WebKitWebProcess memory bloat and UI freezes.
    pub fn prepare_image_data_uri(path: &std::path::Path) -> Option<(String, String)> {
        let Ok(metadata) = std::fs::metadata(path) else {
            return None;
        };
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let (mime, is_jpeg) = match ext.as_str() {
            "png" => ("image/png", false),
            "jpg" | "jpeg" => ("image/jpeg", true),
            "gif" => ("image/gif", false),
            "svg" => ("image/svg+xml", false),
            "webp" => ("image/webp", false),
            _ => ("image/png", false),
        };

        let is_vector_or_anim = ext == "svg" || ext == "gif";
        let file_size = metadata.len();

        if is_vector_or_anim || file_size < 1_500_000 {
            if !is_vector_or_anim {
                if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file(path) {
                    let w = pixbuf.width();
                    let h = pixbuf.height();
                    if w > 1920 || h > 1920 {
                        let scale = 1920.0 / (w.max(h) as f64);
                        let target_w = (w as f64 * scale).round() as i32;
                        let target_h = (h as f64 * scale).round() as i32;
                        if let Some(scaled) = pixbuf.scale_simple(target_w, target_h, gdk_pixbuf::InterpType::Bilinear) {
                            let format = if is_jpeg { "jpeg" } else { "png" };
                            let options: &[(&str, &str)] = if is_jpeg {
                                &[("quality", "88")]
                            } else {
                                &[]
                            };
                            if let Ok(buf) = scaled.save_to_bufferv(format, options) {
                                let b64 = encode_base64(&buf);
                                return Some((format!("data:{mime};base64,{b64}"), mime.to_string()));
                            }
                        }
                    }
                }
            }
            if let Ok(bytes) = std::fs::read(path) {
                let b64 = encode_base64(&bytes);
                return Some((format!("data:{mime};base64,{b64}"), mime.to_string()));
            }
        } else {
            if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file_at_scale(path, 1920, 1920, true) {
                let format = if is_jpeg { "jpeg" } else { "png" };
                let options: &[(&str, &str)] = if is_jpeg {
                    &[("quality", "85")]
                } else {
                    &[]
                };
                if let Ok(buf) = pixbuf.save_to_bufferv(format, options) {
                    let b64 = encode_base64(&buf);
                    return Some((format!("data:{mime};base64,{b64}"), mime.to_string()));
                }
            }
            if let Ok(bytes) = std::fs::read(path) {
                let b64 = encode_base64(&bytes);
                return Some((format!("data:{mime};base64,{b64}"), mime.to_string()));
            }
        }

        None
    }

    pub fn insert_image_from_path(&self, path: &std::path::Path) -> bool {
        if self.plain_text_mode() || !Self::is_supported_image(path) {
            return false;
        }
        let Some((data_uri, _)) = Self::prepare_image_data_uri(path) else {
            return false;
        };
        let js = format!(
            r#"(function() {{
    const uri = '{data_uri}';
    let sel = window.getSelection();
    const editor = document.body;
    const sig = document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');

    function isBlockEmpty(el) {{
        if (!el) return true;
        if (el.nodeType === 3) return el.textContent.replace(/[\s\u200b\u00a0]+/g, '') === '';
        const text = el.textContent ? el.textContent.replace(/[\s\u200b\u00a0]+/g, '') : '';
        if (text === '') {{
            const imgs = el.querySelectorAll ? el.querySelectorAll('img') : [];
            return imgs.length === 0;
        }}
        return false;
    }}

    function isInsideOrAfterSig(node, offset) {{
        if (!sig || !node) return false;
        if (sig.contains(node)) return true;
        try {{
            const cmp = sig.compareDocumentPosition(node);
            if (cmp & Node.DOCUMENT_POSITION_FOLLOWING) return true;
            if (node === editor) {{
                const sigIdx = Array.prototype.indexOf.call(editor.childNodes, sig);
                if (sigIdx !== -1 && offset > sigIdx) return true;
            }}
        }} catch(e) {{}}
        return false;
    }}

    let validSel = sel && sel.rangeCount > 0 && editor.contains(sel.anchorNode);
    if (validSel && sig && isInsideOrAfterSig(sel.anchorNode, sel.anchorOffset)) {{
        validSel = false;
    }}

    const img = document.createElement('img');
    img.src = uri;
    img.style.maxWidth = '100%';
    img.style.height = 'auto';
    img.setAttribute('draggable', 'true');
    img.id = '__meridian_img_' + Date.now() + '_' + Math.floor(Math.random() * 10000);
    img.setAttribute('data-meridian-id', img.id);

    let inserted = false;

    // Check if drop was marked as top
    const wasTop = !!window.__lastDropWasTop;
    if (window.__lastDropWasTop) {{
        window.__lastDropWasTop = false;
        let firstBlock = null;
        for (let i = 0; i < editor.children.length; i++) {{
            const child = editor.children[i];
            if (child === sig || child.id === '__meridian_drop_caret' || child.tagName === 'STYLE' || child.tagName === 'SCRIPT') continue;
            firstBlock = child;
            break;
        }}
        if (firstBlock && isBlockEmpty(firstBlock) && firstBlock !== sig) {{
            firstBlock.innerHTML = '';
            firstBlock.appendChild(img);
            inserted = true;
        }} else {{
            const p = document.createElement('p');
            p.style.margin = '4px 0';
            p.appendChild(img);
            if (firstBlock) {{
                editor.insertBefore(p, firstBlock);
            }} else if (sig) {{
                editor.insertBefore(p, sig);
            }} else {{
                editor.appendChild(p);
            }}
            inserted = true;
        }}
    }}

    if (!inserted && validSel) {{
        const range = sel.getRangeAt(0);
        let container = range.startContainer;
        let block = (container.nodeType === 1 ? container : container.parentElement)?.closest('p, div, li, blockquote');

        if (container === editor) {{
            const childAtOffset = editor.childNodes[range.startOffset];
            const prevChild = range.startOffset > 0 ? editor.childNodes[range.startOffset - 1] : null;
            if (prevChild && isBlockEmpty(prevChild) && prevChild !== sig) {{
                block = prevChild;
            }} else if (childAtOffset && isBlockEmpty(childAtOffset) && childAtOffset !== sig) {{
                block = childAtOffset;
            }}
        }}

        if (block && isBlockEmpty(block) && block !== sig) {{
            block.innerHTML = '';
            block.appendChild(img);
            inserted = true;
        }} else if (container.nodeType === 1 && container.childNodes.length === 1 && container.childNodes[0].nodeName === 'BR') {{
            container.innerHTML = '';
            container.appendChild(img);
            inserted = true;
        }} else {{
            range.deleteContents();
            if (container.nodeType === 1 && container.children.length === 1 && container.children[0].nodeName === 'BR') {{
                container.innerHTML = '';
                container.appendChild(img);
            }} else if (container === editor) {{
                const p = document.createElement('p');
                p.style.margin = '4px 0';
                p.appendChild(img);
                range.insertNode(p);
            }} else {{
                range.insertNode(img);
            }}
            inserted = true;
        }}
    }}

    if (!inserted) {{
        let firstBlock = null;
        for (let i = 0; i < editor.children.length; i++) {{
            const child = editor.children[i];
            if (child === sig || child.id === '__meridian_drop_caret' || child.tagName === 'STYLE' || child.tagName === 'SCRIPT') continue;
            firstBlock = child;
            break;
        }}
        if (firstBlock && isBlockEmpty(firstBlock) && firstBlock !== sig) {{
            firstBlock.innerHTML = '';
            firstBlock.appendChild(img);
        }} else if (sig) {{
            const p = document.createElement('p');
            p.style.margin = '4px 0';
            p.appendChild(img);
            editor.insertBefore(p, sig);
        }} else {{
            const p = document.createElement('p');
            p.style.margin = '4px 0';
            p.appendChild(img);
            editor.appendChild(p);
        }}
    }}

    // Clean up empty first block only if dropped at top
    if (wasTop) {{
        let firstBlock = null;
        for (let i = 0; i < editor.children.length; i++) {{
            const child = editor.children[i];
            if (child === sig || child.id === '__meridian_drop_caret' || child.tagName === 'STYLE' || child.tagName === 'SCRIPT') continue;
            firstBlock = child;
            break;
        }}
        if (firstBlock && firstBlock !== sig && isBlockEmpty(firstBlock) && !firstBlock.contains(img)) {{
            firstBlock.remove();
        }}
    }}

    // Set selection after image
    try {{
        const afterRange = document.createRange();
        afterRange.setStartAfter(img);
        afterRange.collapse(true);
        sel = window.getSelection();
        if (sel) {{
            sel.removeAllRanges();
            sel.addRange(afterRange);
        }}
    }} catch(e) {{}}
}})();"#
        );
        if let Some(wv) = self.imp().web_view.borrow().as_ref() {
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
            true
        } else {
            false
        }
    }

    pub fn connect_image_convert_to_attachment<F>(&self, callback: F)
    where
        F: Fn(crate::services::draft_store_service::DraftAttachment) + 'static,
    {
        if let Ok(mut cbs) = self.imp().image_convert_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub(crate) fn handle_image_menu_script_message(&self, json: &str) {
        let unquoted: String = serde_json::from_str(json).unwrap_or_else(|_| json.to_string());
        let Ok(payload) = serde_json::from_str::<ImageMenuPayload>(&unquoted)
            .or_else(|_| serde_json::from_str::<ImageMenuPayload>(json))
        else {
            return;
        };
        self.present_image_context_popover(&payload);
    }

    fn present_image_context_popover(&self, payload: &ImageMenuPayload) {
        if let Some(old_pop) = self.imp().active_image_popover.borrow_mut().take() {
            old_pop.unparent();
        }

        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };

        let popover = gtk::Popover::builder()
            .has_arrow(true)
            .pointing_to(&gdk::Rectangle::new(payload.x, payload.y, 1, 1))
            .css_classes(vec!["image-options-popover"])
            .build();
        popover.set_parent(&web_view);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_start(8);
        content.set_margin_end(8);
        content.set_margin_top(8);
        content.set_margin_bottom(8);

        // Header
        let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let header_icon = gtk::Image::from_icon_name("insert-image-symbolic");
        header_icon.set_pixel_size(16);
        let header_label = gtk::Label::builder()
            .label(&gettext("Inline-Bild"))
            .css_classes(vec!["heading"])
            .xalign(0.0)
            .build();
        header_box.append(&header_icon);
        header_box.append(&header_label);
        content.append(&header_box);

        // 1. Alignment section
        let align_label = gtk::Label::builder()
            .label(&gettext("Anordnung"))
            .css_classes(vec!["dim-label", "caption"])
            .xalign(0.0)
            .build();
        content.append(&align_label);

        let align_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        align_box.add_css_class("linked");

        let btn_align_left = gtk::Button::builder()
            .icon_name("format-justify-left-symbolic")
            .tooltip_text(&gettext("Linksbündig"))
            .build();
        let btn_align_center = gtk::Button::builder()
            .icon_name("format-justify-center-symbolic")
            .tooltip_text(&gettext("Zentriert"))
            .build();
        let btn_align_right = gtk::Button::builder()
            .icon_name("format-justify-right-symbolic")
            .tooltip_text(&gettext("Rechtsbündig"))
            .build();
        let btn_align_wrap = gtk::Button::builder()
            .icon_name("format-text-direction-ltr-symbolic")
            .tooltip_text(&gettext("Textumfluss links"))
            .build();

        match payload.align.as_str() {
            "left" => btn_align_left.add_css_class("suggested-action"),
            "center" => btn_align_center.add_css_class("suggested-action"),
            "right" => btn_align_right.add_css_class("suggested-action"),
            _ => btn_align_wrap.add_css_class("suggested-action"),
        }

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (b_l, b_c, b_r, b_w) = (
            btn_align_left.clone(),
            btn_align_center.clone(),
            btn_align_right.clone(),
            btn_align_wrap.clone(),
        );
        btn_align_left.connect_clicked(move |_| {
            b_l.add_css_class("suggested-action");
            b_c.remove_css_class("suggested-action");
            b_r.remove_css_class("suggested-action");
            b_w.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.display = 'block'; el.style.marginLeft = '0'; el.style.marginRight = 'auto'; el.style.float = 'none'; el.removeAttribute('align'); }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (b_l, b_c, b_r, b_w) = (
            btn_align_left.clone(),
            btn_align_center.clone(),
            btn_align_right.clone(),
            btn_align_wrap.clone(),
        );
        btn_align_center.connect_clicked(move |_| {
            b_l.remove_css_class("suggested-action");
            b_c.add_css_class("suggested-action");
            b_r.remove_css_class("suggested-action");
            b_w.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.display = 'block'; el.style.marginLeft = 'auto'; el.style.marginRight = 'auto'; el.style.float = 'none'; el.removeAttribute('align'); }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (b_l, b_c, b_r, b_w) = (
            btn_align_left.clone(),
            btn_align_center.clone(),
            btn_align_right.clone(),
            btn_align_wrap.clone(),
        );
        btn_align_right.connect_clicked(move |_| {
            b_l.remove_css_class("suggested-action");
            b_c.remove_css_class("suggested-action");
            b_r.remove_css_class("suggested-action");
            b_w.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.display = 'block'; el.style.marginLeft = 'auto'; el.style.marginRight = '0'; el.style.float = 'none'; el.removeAttribute('align'); }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (b_l, b_c, b_r, b_w) = (
            btn_align_left.clone(),
            btn_align_center.clone(),
            btn_align_right.clone(),
            btn_align_wrap.clone(),
        );
        btn_align_wrap.connect_clicked(move |_| {
            b_l.remove_css_class("suggested-action");
            b_c.remove_css_class("suggested-action");
            b_r.remove_css_class("suggested-action");
            b_w.add_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.display = 'inline'; el.style.float = 'left'; el.style.margin = '4px 14px 6px 0'; el.removeAttribute('align'); }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        align_box.append(&btn_align_left);
        align_box.append(&btn_align_center);
        align_box.append(&btn_align_right);
        align_box.append(&btn_align_wrap);
        content.append(&align_box);

        // 2. Size section
        let size_label = gtk::Label::builder()
            .label(&gettext("Größe"))
            .css_classes(vec!["dim-label", "caption"])
            .xalign(0.0)
            .build();
        content.append(&size_label);

        let size_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        size_box.add_css_class("linked");

        let btn_size_25 = gtk::Button::builder()
            .label("25%")
            .tooltip_text(&gettext("Klein (25%)"))
            .build();
        let btn_size_50 = gtk::Button::builder()
            .label("50%")
            .tooltip_text(&gettext("Mittel (50%)"))
            .build();
        let btn_size_100 = gtk::Button::builder()
            .label("100%")
            .tooltip_text(&gettext("Groß (100%)"))
            .build();
        let btn_size_orig = gtk::Button::builder()
            .label(&gettext("Original"))
            .tooltip_text(&gettext("Originalgröße"))
            .build();

        match payload.size.as_str() {
            "small" => btn_size_25.add_css_class("suggested-action"),
            "medium" => btn_size_50.add_css_class("suggested-action"),
            "large" => btn_size_100.add_css_class("suggested-action"),
            _ => btn_size_orig.add_css_class("suggested-action"),
        }

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (s25, s50, s100, sorig) = (
            btn_size_25.clone(),
            btn_size_50.clone(),
            btn_size_100.clone(),
            btn_size_orig.clone(),
        );
        btn_size_25.connect_clicked(move |_| {
            s25.add_css_class("suggested-action");
            s50.remove_css_class("suggested-action");
            s100.remove_css_class("suggested-action");
            sorig.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.width = '25%'; el.style.maxWidth = '25%'; el.style.height = 'auto'; }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (s25, s50, s100, sorig) = (
            btn_size_25.clone(),
            btn_size_50.clone(),
            btn_size_100.clone(),
            btn_size_orig.clone(),
        );
        btn_size_50.connect_clicked(move |_| {
            s25.remove_css_class("suggested-action");
            s50.add_css_class("suggested-action");
            s100.remove_css_class("suggested-action");
            sorig.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.width = '50%'; el.style.maxWidth = '50%'; el.style.height = 'auto'; }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (s25, s50, s100, sorig) = (
            btn_size_25.clone(),
            btn_size_50.clone(),
            btn_size_100.clone(),
            btn_size_orig.clone(),
        );
        btn_size_100.connect_clicked(move |_| {
            s25.remove_css_class("suggested-action");
            s50.remove_css_class("suggested-action");
            s100.add_css_class("suggested-action");
            sorig.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.width = '100%'; el.style.maxWidth = '100%'; el.style.height = 'auto'; }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (s25, s50, s100, sorig) = (
            btn_size_25.clone(),
            btn_size_50.clone(),
            btn_size_100.clone(),
            btn_size_orig.clone(),
        );
        btn_size_orig.connect_clicked(move |_| {
            s25.remove_css_class("suggested-action");
            s50.remove_css_class("suggested-action");
            s100.remove_css_class("suggested-action");
            sorig.add_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.width = 'auto'; el.style.maxWidth = '100%'; el.style.height = 'auto'; }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        size_box.append(&btn_size_25);
        size_box.append(&btn_size_50);
        size_box.append(&btn_size_100);
        size_box.append(&btn_size_orig);
        content.append(&size_box);

        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let make_action_btn = |icon: &str, label: &str, tooltip: &str, is_destr: bool| {
            let btn = gtk::Button::builder()
                .css_classes(if is_destr {
                    vec!["flat", "destructive-action"]
                } else {
                    vec!["flat"]
                })
                .halign(gtk::Align::Fill)
                .tooltip_text(tooltip)
                .build();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let img = gtk::Image::from_icon_name(icon);
            img.set_pixel_size(16);
            let lbl = gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .hexpand(true)
                .build();
            row.append(&img);
            row.append(&lbl);
            btn.set_child(Some(&row));
            btn
        };

        // Convert to attachment
        let btn_convert = make_action_btn(
            "mail-attachment-symbolic",
            &gettext("In Anhang umwandeln"),
            &gettext("Entfernt das Bild aus dem Text und hängt es als Datei an"),
            false,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_convert.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.convert_image_to_attachment(&img_id, false);
            }
        });
        content.append(&btn_convert);

        // Also attach
        let btn_attach_also = make_action_btn(
            "list-add-symbolic",
            &gettext("Auch als Anhang anhängen"),
            &gettext("Behält das Bild im Text und hängt es zusätzlich als Datei an"),
            false,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_attach_also.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.convert_image_to_attachment(&img_id, true);
            }
        });
        content.append(&btn_attach_also);

        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Copy image
        let btn_copy = make_action_btn(
            "edit-copy-symbolic",
            &gettext("Bild kopieren"),
            &gettext("Kopiert das Bild in die Zwischenablage"),
            false,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_copy.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.copy_image_to_clipboard(&img_id);
            }
        });
        content.append(&btn_copy);

        // Save image as
        let btn_save = make_action_btn(
            "document-save-as-symbolic",
            &gettext("Bild speichern unter…"),
            &gettext("Speichert das Bild auf der Festplatte"),
            false,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_save.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.save_image_as(&img_id);
            }
        });
        content.append(&btn_save);

        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Remove image
        let btn_remove = make_action_btn(
            "user-trash-symbolic",
            &gettext("Bild entfernen"),
            &gettext("Löscht das Bild aus der E-Mail"),
            true,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_remove.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.remove_image(&img_id);
            }
        });
        content.append(&btn_remove);

        let editor_weak = self.downgrade();
        popover.connect_closed(move |p| {
            let p_clone = p.clone();
            let ed_weak = editor_weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(editor) = ed_weak.upgrade() {
                    let mut cur = editor.imp().active_image_popover.borrow_mut();
                    if let Some(active) = cur.as_ref() {
                        if active == &p_clone {
                            p_clone.unparent();
                            *cur = None;
                        }
                    }
                }
            });
        });

        popover.set_child(Some(&content));
        self.imp().active_image_popover.replace(Some(popover.clone()));
        popover.popup();
    }

    pub fn convert_image_to_attachment(&self, img_id: &str, keep_inline: bool) {
        let Some(wv) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        let js = if keep_inline {
            format!(
                "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (!el) return null; return JSON.stringify({{ src: el.src, alt: el.getAttribute('alt') || el.getAttribute('title') || '' }}); }})()",
                id = img_id
            )
        } else {
            format!(
                "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (!el) return null; const data = {{ src: el.src, alt: el.getAttribute('alt') || el.getAttribute('title') || '' }}; const p = el.parentNode; el.remove(); if (p && p !== document.body && p.childNodes.length === 0) {{ p.remove(); }} return JSON.stringify(data); }})()",
                id = img_id
            )
        };

        let editor_weak = self.downgrade();
        wv.evaluate_javascript(
            &js,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(js_val) = result {
                    let s = js_val.to_str();
                    if s != "null" && s != "undefined" && !s.is_empty() {
                        if let Some(editor) = editor_weak.upgrade() {
                            editor.process_converted_image_json(&s);
                        }
                    }
                }
            },
        );
    }

    fn process_converted_image_json(&self, json_str: &str) {
        let parsed_str: String = serde_json::from_str(json_str).unwrap_or_else(|_| json_str.to_string());
        let Ok(data) = serde_json::from_str::<ConvertedImageData>(&parsed_str)
            .or_else(|_| serde_json::from_str::<ConvertedImageData>(json_str))
        else {
            return;
        };
        let Some((bytes, mime, filename)) = parse_image_data_uri_or_path(&data.src, &data.alt) else {
            return;
        };
        let temp_filename = format!("meridian_att_{}_{}", &glib::uuid_string_random()[..8], filename);
        let temp_path = std::env::temp_dir().join(&temp_filename);
        let _ = std::fs::write(&temp_path, &bytes);
        let b64 = encode_base64(&bytes);

        let attachment = crate::services::draft_store_service::DraftAttachment {
            path: temp_path.to_string_lossy().to_string(),
            filename,
            mime_type: mime,
            size: bytes.len() as u64,
            content_id: None,
            embedded_content_base64: b64,
        };

        let cbs: Vec<_> = if let Ok(guard) = self.imp().image_convert_callbacks.try_borrow() {
            guard.0.clone()
        } else {
            Vec::new()
        };
        for cb in cbs {
            let att = attachment.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                cb(att);
            }));
        }
    }

    pub fn copy_image_to_clipboard(&self, img_id: &str) {
        let Some(wv) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        let editor_weak = self.downgrade();
        let js = format!(
            "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); return el ? el.src : null; }})()",
            id = img_id
        );
        wv.evaluate_javascript(
            &js,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(js_val) = result {
                    let s = js_val.to_str();
                    if let Some((bytes, _, _)) = parse_image_data_uri_or_path(&s, "") {
                        let glib_bytes = glib::Bytes::from_owned(bytes);
                        if let Ok(texture) = gdk::Texture::from_bytes(&glib_bytes) {
                            if let Some(editor) = editor_weak.upgrade() {
                                editor.display().clipboard().set_texture(&texture);
                            }
                        }
                    }
                }
            },
        );
    }

    pub fn save_image_as(&self, img_id: &str) {
        let Some(wv) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        let parent = self.root().and_downcast::<gtk::Window>();
        let js = format!(
            "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (!el) return null; return JSON.stringify({{ src: el.src, alt: el.getAttribute('alt') || el.getAttribute('title') || '' }}); }})()",
            id = img_id
        );
        wv.evaluate_javascript(
            &js,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(js_val) = result {
                    let s = js_val.to_str();
                    let parsed_str: String = serde_json::from_str::<String>(&s).unwrap_or_else(|_| s.to_string());
                    let Ok(data) = serde_json::from_str::<ConvertedImageData>(&parsed_str)
                        .or_else(|_| serde_json::from_str::<ConvertedImageData>(&s))
                    else {
                        return;
                    };
                    if let Some((bytes, _, filename)) = parse_image_data_uri_or_path(&data.src, &data.alt) {
                        let file_dialog = gtk::FileDialog::builder()
                            .title(&gettext("Bild speichern unter…"))
                            .initial_name(&filename)
                            .modal(true)
                            .build();
                        file_dialog.save(parent.as_ref(), None::<&gio::Cancellable>, move |res| {
                            if let Ok(file) = res {
                                if let Some(path) = file.path() {
                                    let _ = std::fs::write(&path, &bytes);
                                }
                            }
                        });
                    }
                }
            },
        );
    }

    pub fn remove_image(&self, img_id: &str) {
        let Some(wv) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        let js = format!(
            "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ const p = el.parentNode; el.remove(); if (p && p !== document.body && p.childNodes.length === 0) {{ p.remove(); }} }} }})()",
            id = img_id
        );
        wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
    }

    pub fn cancel_drag_session(&self) {
        if let Some(wv) = self.imp().web_view.borrow().as_ref() {
            wv.evaluate_javascript(
                "if (typeof resetDraggedElement === 'function') resetDraggedElement(); if (isInternalDrag) { try { window.webkit.messageHandlers.meridianDrag.postMessage('internal_end'); } catch(err) {} } hideCaret(); dragCounter = 0; currentDropRange = null; currentDropInfo = null; isInternalDrag = false; draggedElement = null;",
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
        }
    }

    pub(crate) fn handle_drag_script_message(&self, msg: &str) {
        if msg == "enter" {
            let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_motion_callbacks.try_borrow() {
                guard.0.clone()
            } else {
                Vec::new()
            };
            for cb in cbs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    cb(0.0, 100.0);
                }));
            }
        } else if let Some(coords) = msg.strip_prefix("motion:") {
            let mut parts = coords.split(':');
            if let (Some(y_str), Some(h_str)) = (parts.next(), parts.next()) {
                if let (Ok(y), Ok(h)) = (y_str.parse::<f64>(), h_str.parse::<f64>()) {
                    let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_motion_callbacks.try_borrow() {
                        guard.0.clone()
                    } else {
                        Vec::new()
                    };
                    for cb in cbs {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                            cb(y, h);
                        }));
                    }
                }
            }
        } else if msg == "internal_start" {
            let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_internal_callbacks.try_borrow() {
                guard.0.clone()
            } else {
                Vec::new()
            };
            for cb in cbs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    cb(true);
                }));
            }
        } else if msg == "internal_end" {
            let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_internal_callbacks.try_borrow() {
                guard.0.clone()
            } else {
                Vec::new()
            };
            for cb in cbs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    cb(false);
                }));
            }
        } else if msg == "leave" || msg == "drop" || msg == "cancel" {
            let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_leave_callbacks.try_borrow() {
                guard.0.clone()
            } else {
                Vec::new()
            };
            for cb in cbs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    cb();
                }));
            }
        }
    }

    /// Read the current editor contents asynchronously on the GTK main
    /// context.  WebKit owns the DOM, so callers must not try to access it
    /// synchronously or from a worker thread.
    pub fn read_html<F>(&self, callback: F)
    where
        F: FnOnce(Result<String, glib::Error>) + 'static,
    {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            callback(Err(glib::Error::new(
                glib::FileError::Failed,
                "HTML editor is not initialized",
            )));
            return;
        };

        web_view.evaluate_javascript(
            "document.body.innerHTML",
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| callback(result.map(|value| value.to_str().to_string())),
        );
    }

    /// Read the representation that should be persisted or sent.  Plain-text
    /// mode uses `innerText` so line breaks are not accidentally serialized as
    /// WebKit markup, while rich mode retains the sanitized DOM HTML.
    pub fn read_content<F>(&self, callback: F)
    where
        F: FnOnce(Result<EditorContent, glib::Error>) + 'static,
    {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            callback(Err(glib::Error::new(
                glib::FileError::Failed,
                "HTML editor is not initialized",
            )));
            return;
        };
        let plain_text = self.plain_text_mode();
        let expression = if plain_text {
            "document.body.innerText"
        } else {
            "document.body.innerHTML"
        };
        web_view.evaluate_javascript(
            expression,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                callback(result.map(|value| EditorContent {
                    body: value.to_str().to_string(),
                    plain_text,
                }))
            },
        );
    }

    /// Replace the editable DOM contents.  The HTML is encoded as a JSON
    /// string before it is passed to JavaScript so quoted text from a
    /// reply/forward or a restored draft cannot become executable code.
    pub fn set_html(&self, html: &str) {
        let Ok(encoded) = serde_json::to_string(html) else {
            return;
        };
        let script = if self.plain_text_mode() {
            format!(
                "document.body.innerHTML = {encoded}; document.body.textContent = document.body.innerText;"
            )
        } else {
            format!("document.body.innerHTML = {encoded};")
        };
        self.evaluate_or_queue(script);
    }

    /// Replace the editor with literal text, regardless of the current mode.
    pub fn set_text(&self, text: &str) {
        let Ok(encoded) = serde_json::to_string(text) else {
            return;
        };
        self.evaluate_or_queue(format!("document.body.textContent = {encoded};"));
    }

    /// Insert sanitized HTML at the current selection without replacing the
    /// rest of the message.  In plain-text mode the same fragment is reduced
    /// to literal text inside the DOM before it is inserted.
    pub fn insert_html(&self, html: &str) {
        let safe_html = crate::services::html_sanitizer::sanitize_html(html);
        let Ok(encoded) = serde_json::to_string(&safe_html) else {
            return;
        };
        let script = if self.plain_text_mode() {
            format!(
                "const snippet = document.createElement('div'); snippet.innerHTML = {encoded}; document.execCommand('insertText', false, snippet.innerText);"
            )
        } else {
            format!("document.execCommand('insertHTML', false, {encoded});")
        };
        self.evaluate_or_queue(script);
    }

    /// Replace an existing signature in the DOM or insert a new one.
    /// If `signature_html` is empty, any existing signature element is removed.
    pub fn replace_or_insert_signature(&self, signature_html: &str, top_signature: bool) {
        let trimmed = signature_html.trim();
        let is_empty = trimmed.is_empty();
        let safe_html = if is_empty {
            String::new()
        } else {
            crate::services::html_sanitizer::sanitize_html(trimmed)
        };
        let Ok(encoded) = serde_json::to_string(&safe_html) else {
            return;
        };
        let script = format!(
            r#"(function() {{
    const el = document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');
    const isEmpty = {is_empty};
    if (isEmpty) {{
        if (el) {{
            el.remove();
        }}
        return;
    }}
    const temp = document.createElement('div');
    temp.innerHTML = {encoded};
    const newSig = temp.firstElementChild || temp;
    if (el) {{
        el.replaceWith(newSig);
    }} else {{
        if ({top_signature}) {{
            document.body.prepend(newSig);
        }} else {{
            const br = document.createElement('p');
            br.innerHTML = '<br>';
            document.body.appendChild(br);
            document.body.appendChild(newSig);
        }}
    }}
}})();"#
        );
        self.evaluate_or_queue(script);
    }

    pub fn spell_checking_enabled(&self) -> bool {
        self.imp().spell_checking_enabled.get()
    }

    pub fn set_spell_checking_enabled(&self, enabled: bool) {
        self.imp().spell_checking_enabled.set(enabled);
        if let Some(web_context) = self.imp().web_context.borrow().as_ref() {
            web_context.set_spell_checking_enabled(enabled);
        }
    }

    pub fn spell_checking_languages(&self) -> Vec<String> {
        self.imp().spell_checking_languages.borrow().clone()
    }

    pub fn set_spell_checking_languages(&self, languages: &[&str]) {
        self.imp()
            .spell_checking_languages
            .replace(languages.iter().map(|s| s.to_string()).collect());
        if let Some(web_context) = self.imp().web_context.borrow().as_ref() {
            web_context.set_spell_checking_languages(languages);
        }
    }

    pub fn apply_spellcheck_settings(&self, settings: &AppSettings) {
        self.set_spell_checking_enabled(settings.composer_inline_spelling());
        let raw = settings.spellcheck_languages();
        let languages: Vec<&str> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        self.set_spell_checking_languages(&languages);
    }

    pub fn plain_text_mode(&self) -> bool {
        self.imp().plain_text_mode.get()
    }

    /// Switch between rich HTML and literal text while preserving the current
    /// visible content.  The conversion is performed inside the DOM so user
    /// text is never interpolated into JavaScript source.
    pub fn set_plain_text_mode(&self, plain_text: bool) {
        if self.plain_text_mode() == plain_text {
            return;
        }
        self.imp().plain_text_mode.set(plain_text);
        if let Some(toolbar) = self.imp().format_toolbar.borrow().as_ref() {
            toolbar.set_sensitive(!plain_text);
        }
        if plain_text {
            if let Some(revealer) = self.imp().format_toolbar_revealer.borrow().as_ref() {
                revealer.set_reveal_child(false);
            }
        }
        let script = if plain_text {
            "document.body.textContent = document.body.innerText; document.body.style.whiteSpace = 'pre-wrap';"
                .to_string()
        } else {
            concat!(
                "const text = document.body.innerText;",
                "document.body.innerHTML = '';",
                "const lines = text.split(/\\n/);",
                "for (const line of lines) { const p = document.createElement('p'); ",
                "p.textContent = line || '\\u00a0'; document.body.appendChild(p); }",
                "document.body.style.whiteSpace = 'normal';"
            )
            .to_string()
        };
        self.evaluate_or_queue(script);
        self.evaluate_or_queue(format!("window.__plainTextMode = {};", plain_text));
    }

    pub fn is_format_toolbar_visible(&self) -> bool {
        self.imp()
            .format_toolbar_revealer
            .borrow()
            .as_ref()
            .map(|r| r.reveals_child())
            .unwrap_or(false)
    }

    pub fn set_format_toolbar_visible(&self, visible: bool) {
        if let Some(revealer) = self.imp().format_toolbar_revealer.borrow().as_ref() {
            revealer.set_reveal_child(visible);
        }
    }

    pub fn toggle_format_toolbar(&self) -> bool {
        let new_state = !self.is_format_toolbar_visible();
        self.set_format_toolbar_visible(new_state);
        new_state
    }

    pub fn clear_selection(&self) {
        self.evaluate_or_queue(
            "const sel = window.getSelection(); if (sel && !sel.isCollapsed) { sel.collapseToStart(); }".to_string(),
        );
    }

    fn evaluate_or_queue(&self, script: String) {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        if !self.imp().web_view_ready.get() {
            self.imp().pending_scripts.borrow_mut().push(script);
            return;
        }
        web_view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
    }

    fn flush_pending_scripts(&self) {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        self.imp().web_view_ready.set(true);
        let scripts = std::mem::take(&mut *self.imp().pending_scripts.borrow_mut());
        for script in scripts {
            web_view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
        }
    }

    fn build_ui(&self) {
        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        // WYSIWYG Toolbar
        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(16)
            .margin_end(16)
            .margin_top(4)
            .margin_bottom(8)
            .css_classes(vec!["composer-format-toolbar"])
            .build();

        let btn_bold = gtk::Button::builder()
            .icon_name("format-text-bold-symbolic")
            .tooltip_text(&gettext("Bold (Ctrl+B)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_bold, &gettext("Bold"));

        let btn_italic = gtk::Button::builder()
            .icon_name("format-text-italic-symbolic")
            .tooltip_text(&gettext("Italic (Ctrl+I)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_italic, &gettext("Italic"));

        let btn_underline = gtk::Button::builder()
            .icon_name("format-text-underline-symbolic")
            .tooltip_text(&gettext("Underline (Ctrl+U)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_underline, &gettext("Underline"));

        let btn_strikethrough = gtk::Button::builder()
            .icon_name("format-text-strikethrough-symbolic")
            .tooltip_text(&gettext("Strikethrough"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_strikethrough, &gettext("Strikethrough"));

        let btn_clear = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text(&gettext("Clear formatting"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_clear, &gettext("Clear formatting"));

        let format_labels = [
            gettext("Normal"),
            gettext("Heading 1"),
            gettext("Heading 2"),
            gettext("Heading 3"),
            gettext("Preformatted"),
        ];
        let format_strs: Vec<&str> = format_labels.iter().map(|s| s.as_str()).collect();
        let dropdown_format = gtk::DropDown::from_strings(&format_strs);
        dropdown_format.set_valign(gtk::Align::Center);
        set_accessible_label(&dropdown_format, &gettext("Paragraph format"));

        let font_size_labels = [
            gettext("Standardgröße"),
            gettext("10 px (Sehr klein)"),
            gettext("12 px (Klein)"),
            gettext("14 px (Normal)"),
            gettext("16 px (Mittel)"),
            gettext("18 px (Groß)"),
            gettext("24 px (Sehr groß)"),
            gettext("32 px (Riesig)"),
        ];
        let font_size_strs: Vec<&str> = font_size_labels.iter().map(|s| s.as_str()).collect();
        let dropdown_font_size = gtk::DropDown::from_strings(&font_size_strs);
        dropdown_font_size.set_valign(gtk::Align::Center);
        set_accessible_label(&dropdown_font_size, &gettext("Font size"));

        let btn_list = gtk::Button::builder()
            .icon_name("view-list-bullet-symbolic")
            .tooltip_text(&gettext("Bullet list"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_list, &gettext("Bullet list"));

        let btn_ordered_list = gtk::Button::builder()
            .icon_name("view-list-ordered-symbolic")
            .tooltip_text(&gettext("Numbered list"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_ordered_list, &gettext("Numbered list"));

        let btn_blockquote = gtk::Button::builder()
            .icon_name("format-text-direction-ltr-symbolic")
            .tooltip_text(&gettext("Block quote"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_blockquote, &gettext("Block quote"));

        let link_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(10)
            .margin_end(10)
            .margin_top(8)
            .margin_bottom(8)
            .width_request(260)
            .build();
        link_popover_box.append(
            &gtk::Label::builder()
                .label(&gettext("Insert hyperlink"))
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading"])
                .build(),
        );
        let entry_url = gtk::Entry::builder()
            .placeholder_text("https://example.com")
            .build();
        let entry_label = gtk::Entry::builder()
            .placeholder_text(&gettext("Link text (optional)"))
            .build();
        let btn_insert_link_confirm = gtk::Button::builder()
            .label(&gettext("Insert"))
            .css_classes(vec!["suggested-action"])
            .build();
        link_popover_box.append(&entry_url);
        link_popover_box.append(&entry_label);
        link_popover_box.append(&btn_insert_link_confirm);

        let btn_confirm_url = btn_insert_link_confirm.clone();
        entry_url.connect_activate(move |_| {
            btn_confirm_url.emit_clicked();
        });
        let btn_confirm_label = btn_insert_link_confirm.clone();
        entry_label.connect_activate(move |_| {
            btn_confirm_label.emit_clicked();
        });

        let link_popover = gtk::Popover::builder().child(&link_popover_box).build();
        let btn_link = gtk::MenuButton::builder()
            .icon_name("insert-link-symbolic")
            .tooltip_text(&gettext("Insert hyperlink"))
            .popover(&link_popover)
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_link, &gettext("Insert hyperlink"));

        let btn_rule = gtk::Button::builder()
            .icon_name("insert-object-symbolic")
            .tooltip_text(&gettext("Insert horizontal rule"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_rule, &gettext("Insert horizontal rule"));

        let btn_image = gtk::Button::builder()
            .icon_name("insert-image-symbolic")
            .tooltip_text(&gettext("Insert image"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_image, &gettext("Insert image"));

        let btn_undo = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text(&gettext("Undo (Ctrl+Z)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_undo, &gettext("Undo"));

        let btn_redo = gtk::Button::builder()
            .icon_name("edit-redo-symbolic")
            .tooltip_text(&gettext("Redo (Ctrl+Shift+Z)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&btn_redo, &gettext("Redo"));

        toolbar.append(&dropdown_format);
        toolbar.append(&dropdown_font_size);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_bold);
        toolbar.append(&btn_italic);
        toolbar.append(&btn_underline);
        toolbar.append(&btn_strikethrough);
        toolbar.append(&btn_clear);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_list);
        toolbar.append(&btn_ordered_list);
        toolbar.append(&btn_blockquote);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_link);
        toolbar.append(&btn_image);
        toolbar.append(&btn_rule);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_undo);
        toolbar.append(&btn_redo);

        self.imp().format_toolbar.replace(Some(toolbar.clone()));

        let toolbar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(false)
            .propagate_natural_height(true)
            .min_content_width(0)
            .css_classes(vec!["format-toolbar-scrolled"])
            .child(&toolbar)
            .build();

        let toolbar_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        toolbar_container.append(&toolbar_scroll);

        let toolbar_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(180)
            .reveal_child(false)
            .child(&toolbar_container)
            .build();

        self.imp()
            .format_toolbar_revealer
            .replace(Some(toolbar_revealer.clone()));

        vbox.append(&toolbar_revealer);

        // WebKit6 WebView Canvas
        let web_context = webkit6::WebContext::new();
        let inline_spelling = AppSettings::try_new()
            .map(|settings| settings.composer_inline_spelling())
            .unwrap_or(true);
        web_context.set_spell_checking_enabled(inline_spelling);
        self.imp().spell_checking_enabled.set(inline_spelling);

        let initial_languages = AppSettings::try_new()
            .map(|settings| {
                settings
                    .spellcheck_languages()
                    .split(',')
                    .map(str::trim)
                    .filter(|language| !language.is_empty())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let initial_lang_refs: Vec<&str> = initial_languages.iter().map(String::as_str).collect();
        web_context.set_spell_checking_languages(&initial_lang_refs);
        self.imp().spell_checking_languages.replace(initial_languages);

        self.imp().web_context.replace(Some(web_context.clone()));

        if let Some(settings) = AppSettings::try_new() {
            let editor_weak = self.downgrade();
            let id_spelling = settings.connect_changed(Some("composer-inline-spelling"), move |s, _| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_spell_checking_enabled(s.boolean("composer-inline-spelling"));
                }
            });
            let editor_weak = self.downgrade();
            let id_languages = settings.connect_changed(Some("spellcheck-languages"), move |s, _| {
                if let Some(editor) = editor_weak.upgrade() {
                    let raw = s.string("spellcheck-languages");
                    let langs: Vec<&str> = raw
                        .split(',')
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .collect();
                    editor.set_spell_checking_languages(&langs);
                }
            });
            self.imp().spelling_handler_ids.borrow_mut().push(id_spelling);
            self.imp().spelling_handler_ids.borrow_mut().push(id_languages);
        }

        let user_content_manager = webkit6::UserContentManager::new();
        user_content_manager.register_script_message_handler("meridianDrag", None);
        user_content_manager.register_script_message_handler("meridianImageMenu", None);

        let editor_weak_for_drag = self.downgrade();
        user_content_manager.connect_local(
            "script-message-received::meridianDrag",
            false,
            move |values| {
                if let Some(editor) = editor_weak_for_drag.upgrade() {
                    if let Some(js_val) = values
                        .get(1)
                        .and_then(|v| v.get::<javascriptcore6::Value>().ok())
                    {
                        let msg = js_val.to_str();
                        editor.handle_drag_script_message(&msg);
                    }
                }
                None
            },
        );

        let editor_weak_for_img_menu = self.downgrade();
        user_content_manager.connect_local(
            "script-message-received::meridianImageMenu",
            false,
            move |values| {
                if let Some(editor) = editor_weak_for_img_menu.upgrade() {
                    if let Some(js_val) = values
                        .get(1)
                        .and_then(|v| v.get::<javascriptcore6::Value>().ok())
                    {
                        let msg = js_val.to_str();
                        editor.handle_image_menu_script_message(&msg);
                    }
                }
                None
            },
        );

        let web_view = webkit6::WebView::builder()
            .web_context(&web_context)
            .user_content_manager(&user_content_manager)
            .vexpand(true)
            .hexpand(true)
            .build();
        web_view.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        set_accessible_label(&web_view, &gettext("Email message body"));

        web_view.connect_context_menu(|_, _, hit_test_result| {
            if hit_test_result.context_is_image() {
                return true;
            }
            false
        });

        if let Some(settings) = webkit6::prelude::WebViewExt::settings(&web_view) {
            let desktop_font = crate::views::mail_viewer::detect_desktop_font_family();
            let desktop_mono = crate::views::mail_viewer::detect_desktop_monospace_font_family();
            settings.set_default_font_family(&desktop_font);
            settings.set_sans_serif_font_family(&desktop_font);
            settings.set_serif_font_family(&desktop_font);
            settings.set_monospace_font_family(&desktop_mono);
            settings.set_enable_javascript(true);
            settings.set_auto_load_images(true);
            settings.set_enable_developer_extras(false);
            settings.set_allow_file_access_from_file_urls(false);
            settings.set_allow_universal_access_from_file_urls(false);
            settings.set_enable_webgl(false);
            settings.set_enable_webrtc(false);
            settings.set_enable_mediasource(false);
            settings.set_enable_media_stream(false);
            settings.set_enable_mock_capture_devices(false);
            settings.set_enable_html5_local_storage(false);
            settings.set_enable_html5_database(false);
            settings.set_enable_offline_web_application_cache(false);
        }

        let font_family_raw = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::mail_font_family)
            .unwrap_or_else(|| "system-ui, sans-serif".to_string());
        let font_family = crate::views::mail_viewer::safe_css_font_family(&font_family_raw);
        let font_size = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::mail_font_size)
            .unwrap_or(14);
        let initial_html = format!(
            concat!(
                "<!DOCTYPE html><html><head><style>",
                "html, body {{ font-family: {}; font-size: {}px; line-height: 1.5; color: #222; margin: 12px; background-color: transparent; }}",
                "img {{ max-width: 100%; height: auto; cursor: grab; -webkit-user-drag: element; }}",
                "img:active {{ cursor: grabbing; }}",
                "a {{ color: #1a73e8; text-decoration: underline; cursor: pointer; }}",
                "blockquote, blockquote[type=\"cite\"] {{ margin: 4px 0 4px 0.8ex; border-left: 2px solid #729fcf; padding-left: 1ex; }}",
                "blockquote > :first-child {{ margin-top: 0; }}",
                "@media (prefers-color-scheme: dark) {{ html, body {{ color: #eee; background-color: transparent; }} a {{ color: #78aeed; }} blockquote, blockquote[type=\"cite\"] {{ border-left-color: #3584e4; }} }}",
                "</style>",
                "<script>",
                "document.addEventListener('mouseover', function(e) {{",
                "    const link = e.target.closest('a');",
                "    if (link && link.href && !link.title) {{",
                "        link.title = link.href + ' (Strg+Klick zum Öffnen)';",
                "    }}",
                "}});",
                "document.addEventListener('click', function(e) {{",
                "    const link = e.target.closest('a');",
                "    if (link && link.href && (e.ctrlKey || e.metaKey)) {{",
                "        e.preventDefault();",
                "        window.open(link.href, '_blank');",
                "    }}",
                "}});",
                "document.addEventListener('paste', function(e) {{",
                "    if (!e.clipboardData) return;",
                "    const items = e.clipboardData.items;",
                "    if (items) {{",
                "        for (let i = 0; i < items.length; i++) {{",
                "            if (items[i].type.indexOf('image') !== -1) {{",
                "                const file = items[i].getAsFile();",
                "                if (file) {{",
                "                    e.preventDefault();",
                "                    const reader = new FileReader();",
                "                    reader.onload = function(evt) {{",
                "                        const img = new Image();",
                "                        img.onload = function() {{",
                "                            if (img.width > 1920 || img.height > 1920) {{",
                "                                const scale = 1920 / Math.max(img.width, img.height);",
                "                                const cv = document.createElement('canvas');",
                "                                cv.width = Math.round(img.width * scale);",
                "                                cv.height = Math.round(img.height * scale);",
                "                                const ctx = cv.getContext('2d');",
                "                                ctx.drawImage(img, 0, 0, cv.width, cv.height);",
                "                                document.execCommand('insertImage', false, cv.toDataURL('image/jpeg', 0.85));",
                "                            }} else {{",
                "                                document.execCommand('insertImage', false, evt.target.result);",
                "                            }}",
                "                        }};",
                "                        img.src = evt.target.result;",
                "                    }};",
                "                    reader.readAsDataURL(file);",
                "                    return;",
                "                }}",
                "            }}",
                "        }}",
                "    }}",
                "    const types = Array.from(e.clipboardData.types || []);",
                "    if (types.includes('text/html')) {{",
                "        const rawHtml = e.clipboardData.getData('text/html');",
                "        if (rawHtml) {{",
                "            e.preventDefault();",
                "            let cleanHtml = rawHtml;",
                "            try {{",
                "                const parser = new DOMParser();",
                "                const doc = parser.parseFromString(rawHtml, 'text/html');",
                "                const isWhite = function(v) {{",
                "                    if (!v) return false;",
                "                    const s = v.toLowerCase().replace(/\\s+/g, '');",
                "                    return s === 'rgb(255,255,255)' || s === 'rgba(255,255,255,1)' || s === '#ffffff' || s === '#fff' || s === 'white' || s === 'transparent' || s === 'rgba(0,0,0,0)';",
                "                }};",
                "                const all = doc.querySelectorAll('*');",
                "                for (let j = 0; j < all.length; j++) {{",
                "                    const el = all[j];",
                "                    if (isWhite(el.style.backgroundColor)) {{ el.style.removeProperty('background-color'); }}",
                "                    if (isWhite(el.style.background)) {{ el.style.removeProperty('background'); }}",
                "                    if (el.style.caretColor) {{ el.style.removeProperty('caret-color'); }}",
                "                    if (el.hasAttribute('bgcolor') && isWhite(el.getAttribute('bgcolor'))) {{ el.removeAttribute('bgcolor'); }}",
                "                    if (el.getAttribute('style') !== null && el.style.length === 0) {{ el.removeAttribute('style'); }}",
                "                }}",
                "                cleanHtml = doc.body.innerHTML;",
                "            }} catch (err) {{",
                "                cleanHtml = rawHtml",
                "                    .replace(/(?:background-color|background)\\s*:\\s*(?:rgb\\(\\s*255\\s*,\\s*255\\s*,\\s*255\\s*\\)|rgba\\(\\s*255\\s*,\\s*255\\s*,\\s*255\\s*,\\s*1(?:\\.0+)?\\s*\\)|#(?:ffffff|fff)\\b|white\\b)\\s*(?:!\\s*important)?\\s*;?/gi, '')",
                "                    .replace(/caret-color\\s*:\\s*[^;\"]+;?/gi, '')",
                "                    .replace(/\\s*bgcolor\\s*=\\s*[\"']\\s*(?:#ffffff|#fff|white|rgb\\(\\s*255\\s*,\\s*255\\s*,\\s*255\\s*\\))\\s*[\"']/gi, '');",
                "            }}",
                "            if (cleanHtml && cleanHtml.trim().length > 0) {{",
                "                document.execCommand('insertHTML', false, cleanHtml);",
                "                return;",
                "            }}",
                "        }}",
                "    }}",
                "    const text = e.clipboardData.getData('text/plain');",
                "    if (text) {{",
                "        const urlRegex = /(https?:\\/\\/[^\\s<]+|mailto:[^\\s<]+|www\\.[a-zA-Z0-9.\\-]+\\.[a-zA-Z]{{2,}}[^\\s<]*|[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{{2,}})/g;",
                "        if (urlRegex.test(text)) {{",
                "            e.preventDefault();",
                "            const escaped = text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');",
                "            const htmlWithLinks = escaped.replace(urlRegex, function(match) {{",
                "                let cleanMatch = match;",
                "                let trailing = '';",
                "                while (/[.,;:)]$/.test(cleanMatch)) {{",
                "                    trailing = cleanMatch.slice(-1) + trailing;",
                "                    cleanMatch = cleanMatch.slice(0, -1);",
                "                }}",
                "                let href = cleanMatch;",
                "                if (cleanMatch.startsWith('www.')) {{",
                "                    href = 'https://' + cleanMatch;",
                "                }} else if (cleanMatch.includes('@') && !cleanMatch.startsWith('mailto:') && !cleanMatch.startsWith('http')) {{",
                "                    href = 'mailto:' + cleanMatch;",
                "                }}",
                "                return '<a href=\"' + href + '\">' + cleanMatch + '</a>' + trailing;",
                "            }}).replace(/\\r?\\n/g, '<br>');",
                "            document.execCommand('insertHTML', false, htmlWithLinks);",
                "        }}",
                "    }}",
                "}});",
                "window.__plainTextMode = false;",
                "document.addEventListener('contextmenu', function(e) {{",
                "    try {{",
                "        const target = e.target;",
                "        let img = null;",
                "        if (target) {{",
                "            if (target.nodeName === 'IMG') {{",
                "                img = target;",
                "            }} else if (target.closest) {{",
                "                img = target.closest('img');",
                "            }}",
                "            if (!img && target.querySelector) {{",
                "                img = target.querySelector('img');",
                "            }}",
                "        }}",
                "        if (img && !window.__plainTextMode) {{",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            if (!img.id) {{",
                "                img.id = '__meridian_img_' + Date.now() + '_' + Math.floor(Math.random() * 10000);",
                "            }}",
                "            if (!img.getAttribute('data-meridian-id')) {{",
                "                img.setAttribute('data-meridian-id', img.id);",
                "            }}",
                "        const rect = img.getBoundingClientRect();",
                "        let align = 'inline';",
                "        if (img.style.display === 'block') {{",
                "            if (img.style.marginLeft === 'auto' && img.style.marginRight === 'auto') {{",
                "                align = 'center';",
                "            }} else if (img.style.marginLeft === 'auto' || img.getAttribute('align') === 'right') {{",
                "                align = 'right';",
                "            }} else {{",
                "                align = 'left';",
                "            }}",
                "        }} else if (img.style.float === 'left') {{",
                "            align = 'wrap_left';",
                "        }} else if (img.style.float === 'right') {{",
                "            align = 'wrap_right';",
                "        }} else if (img.getAttribute('align') === 'left') {{",
                "            align = 'left';",
                "        }} else if (img.getAttribute('align') === 'right') {{",
                "            align = 'right';",
                "        }} else if (img.getAttribute('align') === 'center') {{",
                "            align = 'center';",
                "        }}",
                "        let size = 'original';",
                "        const w = img.style.width || img.style.maxWidth || '';",
                "        if (w === '25%') size = 'small';",
                "        else if (w === '50%') size = 'medium';",
                "        else if (w === '100%') size = 'large';",
                "        const payload = JSON.stringify({{",
                "            id: img.id,",
                "            x: Math.max(0, Math.round(e.clientX)),",
                "            y: Math.max(0, Math.round(e.clientY)),",
                "            img_x: Math.round(rect.left),",
                "            img_y: Math.round(rect.top),",
                "            img_w: Math.round(rect.width),",
                "            img_h: Math.round(rect.height),",
                "            align: align,",
                "            size: size",
                "        }});",
                "        try {{",
                "            window.webkit.messageHandlers.meridianImageMenu.postMessage(payload);",
                "        }} catch(err) {{}}",
                "        return false;",
                "    }}",
                "    }} catch(err) {{}}",
                "}});",
                "let lastDragNotify = 0;",
                "let isInternalDrag = false;",
                "let draggedElement = null;",
                "let currentDropRange = null;",
                "let currentDropInfo = null;",
                "let lastValidDropRange = null;",
                "let lastValidDropInfo = null;",
                "let dropIndicator = null;",
                "function resetDraggedElement() {{",
                "    if (draggedElement) {{",
                "        try {{",
                "            draggedElement.style.opacity = '';",
                "            draggedElement.style.pointerEvents = '';",
                "            if (draggedElement.parentElement && draggedElement.parentElement !== document.body) {{",
                "                draggedElement.parentElement.style.pointerEvents = '';",
                "            }}",
                "        }} catch(e) {{}}",
                "    }}",
                "}};",
                "document.addEventListener('dragstart', function(e) {{",
                "    const img = (e.target && e.target.nodeName === 'IMG') ? e.target : (e.target.querySelector ? e.target.querySelector('img') : (e.target.closest ? e.target.closest('img') : null));",
                "    if (img) {{",
                "        isInternalDrag = true;",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('internal_start'); }} catch(err) {{}}",
                "        draggedElement = img;",
                "        if (!img.id) {{",
                "            img.id = '__meridian_img_' + Date.now() + '_' + Math.floor(Math.random() * 10000);",
                "        }}",
                "        if (!img.getAttribute('data-meridian-id')) {{",
                "            img.setAttribute('data-meridian-id', img.id);",
                "        }}",
                "        if (e.dataTransfer) {{",
                "            e.dataTransfer.effectAllowed = 'move';",
                "            try {{",
                "                e.dataTransfer.setData('text/plain', '');",
                "            }} catch(err) {{}}",
                "            try {{",
                "                const rect = img.getBoundingClientRect();",
                "                const clickX = Math.max(0, Math.min(Math.round(rect.width), Math.round(e.clientX - rect.left)));",
                "                const clickY = Math.max(0, Math.min(Math.round(rect.height), Math.round(e.clientY - rect.top)));",
                "                if (e.dataTransfer.setDragImage) {{",
                "                    e.dataTransfer.setDragImage(img, clickX, clickY);",
                "                }}",
                "            }} catch(err) {{}}",
                "        }}",
                "        setTimeout(() => {{",
                "            if (draggedElement === img) {{",
                "                img.style.opacity = '0.35';",
                "                img.style.pointerEvents = 'none';",
                "                if (img.parentElement && img.parentElement !== document.body) {{",
                "                    img.parentElement.style.pointerEvents = 'none';",
                "                }}",
                "            }}",
                "        }}, 0);",
                "    }} else {{",
                "        isInternalDrag = false;",
                "        draggedElement = null;",
                "    }}",
                "}});",
                "function handleDragEnd() {{",
                "    resetDraggedElement();",
                "    if (isInternalDrag) {{",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('internal_end'); }} catch(err) {{}}",
                "    }}",
                "    isInternalDrag = false;",
                "    draggedElement = null;",
                "    currentDropInfo = null;",
                "    currentDropRange = null;",
                "    lastValidDropInfo = null;",
                "    lastValidDropRange = null;",
                "    dragCounter = 0;",
                "    hideCaret();",
                "}};",
                "document.addEventListener('dragend', handleDragEnd);",
                "window.addEventListener('dragend', handleDragEnd);",
                "function checkHasFiles(e) {{",
                "    if (isInternalDrag) return false;",
                "    if (!e.dataTransfer || !e.dataTransfer.types) return false;",
                "    for (let i = 0; i < e.dataTransfer.types.length; i++) {{",
                "        const t = e.dataTransfer.types[i];",
                "        if (t === 'Files' || t === 'application/x-moz-file' || t === 'text/uri-list') {{",
                "            return true;",
                "        }}",
                "    }}",
                "    return false;",
                "}};",
                "function getDropIndicator() {{",
                "    if (!dropIndicator || !dropIndicator.parentNode) {{",
                "        dropIndicator = document.createElement('div');",
                "        dropIndicator.id = '__meridian_drop_caret';",
                "        dropIndicator.style.position = 'fixed';",
                "        dropIndicator.style.width = '2px';",
                "        dropIndicator.style.backgroundColor = '#3584e4';",
                "        dropIndicator.style.borderRadius = '1px';",
                "        dropIndicator.style.pointerEvents = 'none';",
                "        dropIndicator.style.zIndex = '2147483647';",
                "        dropIndicator.style.boxShadow = '0 0 3px rgba(53, 132, 228, 0.8)';",
                "        dropIndicator.style.animation = 'none';",
                "        dropIndicator.style.display = 'none';",
                "        document.body.appendChild(dropIndicator);",
                "    }}",
                "    return dropIndicator;",
                "}};",
                "function hideCaret() {{",
                "    const el = document.getElementById('__meridian_drop_caret');",
                "    if (el) el.style.display = 'none';",
                "}};",
                "function getCaretRect(range, y) {{",
                "    if (!range) return null;",
                "    let rect = range.getBoundingClientRect();",
                "    if (rect && rect.height > 0 && (rect.top > 0 || rect.bottom > 0)) {{",
                "        return rect;",
                "    }}",
                "    const rects = range.getClientRects();",
                "    if (rects && rects.length > 0 && rects[0].height > 0) {{",
                "        return rects[0];",
                "    }}",
                "    try {{",
                "        const container = range.startContainer;",
                "        const offset = range.startOffset;",
                "        if (container.nodeType === 3) {{",
                "            const len = container.nodeValue ? container.nodeValue.length : 0;",
                "            if (len > 0) {{",
                "                if (offset < len) {{",
                "                    const tr = document.createRange();",
                "                    tr.setStart(container, offset);",
                "                    tr.setEnd(container, offset + 1);",
                "                    const cr = tr.getBoundingClientRect();",
                "                    if (cr && cr.height > 0) {{",
                "                        return {{ left: cr.left, right: cr.left, top: cr.top, bottom: cr.bottom, width: 0, height: cr.height }};",
                "                    }}",
                "                }}",
                "                if (offset > 0) {{",
                "                    const tr = document.createRange();",
                "                    tr.setStart(container, offset - 1);",
                "                    tr.setEnd(container, offset);",
                "                    const cr = tr.getBoundingClientRect();",
                "                    if (cr && cr.height > 0) {{",
                "                        return {{ left: cr.right, right: cr.right, top: cr.top, bottom: cr.bottom, width: 0, height: cr.height }};",
                "                    }}",
                "                }}",
                "            }}",
                "        }} else if (container.nodeType === 1) {{",
                "            if (offset < container.childNodes.length) {{",
                "                const child = container.childNodes[offset];",
                "                if (child && child.getBoundingClientRect) {{",
                "                    const cr = child.getBoundingClientRect();",
                "                    if (cr && cr.height > 0) {{",
                "                        const h = 20;",
                "                        const t = (cr.height > 24 && y !== undefined) ? Math.max(cr.top, Math.min(cr.bottom - h, y - h / 2)) : cr.top;",
                "                        return {{ left: cr.left, right: cr.left, top: t, bottom: t + h, width: 0, height: h }};",
                "                    }}",
                "                }}",
                "            }}",
                "            if (offset > 0) {{",
                "                const child = container.childNodes[offset - 1];",
                "                if (child && child.getBoundingClientRect) {{",
                "                    const cr = child.getBoundingClientRect();",
                "                    if (cr && cr.height > 0) {{",
                "                        const h = 20;",
                "                        const t = (cr.height > 24 && y !== undefined) ? Math.max(cr.top, Math.min(cr.bottom - h, y - h / 2)) : cr.top;",
                "                        return {{ left: cr.right, right: cr.right, top: t, bottom: t + h, width: 0, height: h }};",
                "                    }}",
                "                }}",
                "            }}",
                "            const pr = container.getBoundingClientRect();",
                "            if (pr && pr.height > 0) {{",
                "                const h = 20;",
                "                const t = (pr.height > 24 && y !== undefined) ? Math.max(pr.top, Math.min(pr.bottom - h, y - h / 2)) : pr.top;",
                "                return {{ left: pr.left, right: pr.left, top: t, bottom: t + h, width: 0, height: h }};",
                "            }}",
                "        }}",
                "    }} catch(e) {{}}",
                "    return null;",
                "}};",
                "function getSignatureElement() {{",
                "    return document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');",
                "}};",
                "function isBlockEmpty(el) {{",
                "    if (!el) return true;",
                "    if (el.nodeType === 3) {{",
                "        return el.textContent.replace(/[\\s\\u200b\\u00a0]+/g, '') === '';",
                "    }}",
                "    const text = el.textContent ? el.textContent.replace(/[\\s\\u200b\\u00a0]+/g, '') : '';",
                "    if (text === '') {{",
                "        const imgs = el.querySelectorAll ? el.querySelectorAll('img') : [];",
                "        return imgs.length === 0;",
                "    }}",
                "    return false;",
                "}};",
                "function getContentBlocks(excludeEl) {{",
                "    const sig = getSignatureElement();",
                "    const blocks = [];",
                "    for (let i = 0; i < document.body.children.length; i++) {{",
                "        const child = document.body.children[i];",
                "        if (child === sig || child.id === '__meridian_drop_caret' || child.tagName === 'STYLE' || child.tagName === 'SCRIPT') continue;",
                "        if (excludeEl && (child === excludeEl || child.contains(excludeEl))) continue;",
                "        blocks.push(child);",
                "    }}",
                "    return blocks;",
                "}};",
                "function constrainRangeBeforeSignature(range) {{",
                "    if (!range) return null;",
                "    const sig = getSignatureElement();",
                "    if (!sig) return range;",
                "    let node = range.startContainer;",
                "    let isAfterOrInside = (node === sig || sig.contains(node));",
                "    if (!isAfterOrInside) {{",
                "        try {{",
                "            const cmp = sig.compareDocumentPosition(node);",
                "            if (cmp & Node.DOCUMENT_POSITION_FOLLOWING) {{",
                "                isAfterOrInside = true;",
                "            }} else if (node === document.body) {{",
                "                const sigIdx = Array.prototype.indexOf.call(document.body.childNodes, sig);",
                "                if (sigIdx !== -1 && range.startOffset > sigIdx) {{",
                "                    isAfterOrInside = true;",
                "                }}",
                "            }}",
                "        }} catch(e) {{}}",
                "    }}",
                "    if (isAfterOrInside) {{",
                "        let prev = sig.previousElementSibling;",
                "        if (!prev) {{",
                "            prev = document.createElement('p');",
                "            prev.innerHTML = '<br>';",
                "            document.body.insertBefore(prev, sig);",
                "        }}",
                "        const safeRange = document.createRange();",
                "        safeRange.selectNodeContents(prev);",
                "        safeRange.collapse(false);",
                "        return safeRange;",
                "    }}",
                "    return range;",
                "}};",
                "function updateCaretToPoint(x, y) {{",
                "    if (window.__plainTextMode) return null;",
                "    try {{",
                "        const blocks = getContentBlocks(draggedElement);",
                "        const firstBlock = blocks.length > 0 ? blocks[0] : null;",
                "        const fbRect = firstBlock ? firstBlock.getBoundingClientRect() : null;",
                "        const isNearTop = fbRect",
                "            ? (isBlockEmpty(firstBlock) ? (y <= Math.max(fbRect.bottom, 40)) : (y < fbRect.top - 2))",
                "            : (y <= 40);",
                "        if (firstBlock && isNearTop) {{",
                "            currentDropInfo = {{ isTop: true, block: firstBlock }};",
                "            currentDropRange = null;",
                "            lastValidDropInfo = currentDropInfo;",
                "            lastValidDropRange = null;",
                "            const el = getDropIndicator();",
                "            const caretH = 20;",
                "            const top = Math.round(fbRect.top + (fbRect.height > 20 ? (fbRect.height - 20) / 2 : 0));",
                "            el.style.left = Math.round(fbRect.left) + 'px';",
                "            el.style.top = top + 'px';",
                "            el.style.width = '2px';",
                "            el.style.height = caretH + 'px';",
                "            el.style.display = 'block';",
                "            return currentDropInfo;",
                "        }}",
                "        currentDropInfo = null;",
                "        let range = null;",
                "        if (document.caretRangeFromPoint) {{",
                "            range = document.caretRangeFromPoint(x, y);",
                "        }} else if (document.caretPositionFromPoint) {{",
                "            const pos = document.caretPositionFromPoint(x, y);",
                "            if (pos) {{",
                "                range = document.createRange();",
                "                range.setStart(pos.offsetNode, pos.offset);",
                "                range.collapse(true);",
                "            }}",
                "        }}",
                "        if (!range && blocks.length > 0) {{",
                "            const lastBlock = blocks[blocks.length - 1];",
                "            const lbRect = lastBlock.getBoundingClientRect();",
                "            if (y >= lbRect.bottom) {{",
                "                const fallbackRange = document.createRange();",
                "                fallbackRange.selectNodeContents(lastBlock);",
                "                fallbackRange.collapse(false);",
                "                range = fallbackRange;",
                "            }}",
                "        }}",
                "        if (range) {{",
                "            range = constrainRangeBeforeSignature(range);",
                "            if (draggedElement && (draggedElement.contains(range.startContainer) || draggedElement === range.startContainer || (draggedElement.parentElement && draggedElement.parentElement === range.startContainer && draggedElement.parentElement.children.length === 1))) {{",
                "                hideCaret();",
                "                return null;",
                "            }}",
                "            const rect = getCaretRect(range, y);",
                "            if (rect && rect.height > 0) {{",
                "                const el = getDropIndicator();",
                "                const caretH = 20;",
                "                const top = Math.round(rect.top + (rect.height > 20 ? (rect.height - 20) / 2 : 0));",
                "                el.style.left = Math.round(rect.left) + 'px';",
                "                el.style.top = top + 'px';",
                "                el.style.width = '2px';",
                "                el.style.height = caretH + 'px';",
                "                el.style.display = 'block';",
                "                currentDropRange = range.cloneRange();",
                "                lastValidDropRange = currentDropRange;",
                "                lastValidDropInfo = null;",
                "                return currentDropRange;",
                "            }}",
                "        }}",
                "    }} catch(err) {{}}",
                "    hideCaret();",
                "    return null;",
                "}};",
                "let dragCounter = 0;",
                "document.addEventListener('dragenter', function(e) {{",
                "    if (isInternalDrag) {{",
                "        updateCaretToPoint(e.clientX, e.clientY);",
                "        return;",
                "    }}",
                "    if (checkHasFiles(e)) {{",
                "        dragCounter++;",
                "        updateCaretToPoint(e.clientX, e.clientY);",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('enter'); }} catch(err) {{}}",
                "    }}",
                "}});",
                "document.addEventListener('dragover', function(e) {{",
                "    if (isInternalDrag) {{",
                "        e.preventDefault();",
                "        if (e.dataTransfer) {{ e.dataTransfer.dropEffect = 'move'; }}",
                "        updateCaretToPoint(e.clientX, e.clientY);",
                "        return;",
                "    }}",
                "    if (!checkHasFiles(e)) return;",
                "    e.preventDefault();",
                "    if (e.dataTransfer) {{ e.dataTransfer.dropEffect = 'copy'; }}",
                "    updateCaretToPoint(e.clientX, e.clientY);",
                "    const now = Date.now();",
                "    if (now - lastDragNotify > 40) {{",
                "        lastDragNotify = now;",
                "        try {{",
                "            window.webkit.messageHandlers.meridianDrag.postMessage('motion:' + Math.round(e.clientY) + ':' + Math.round(window.innerHeight));",
                "        }} catch(err) {{}}",
                "    }}",
                "}});",
                "document.addEventListener('dragleave', function(e) {{",
                "    dragCounter--;",
                "    const isOutOfWindow = e.clientY <= 0 || e.clientX <= 0 || e.clientX >= window.innerWidth || e.clientY >= window.innerHeight;",
                "    if (dragCounter <= 0 || isOutOfWindow) {{",
                "        dragCounter = 0;",
                "        hideCaret();",
                "        if (!isInternalDrag) {{",
                "            try {{ window.webkit.messageHandlers.meridianDrag.postMessage('leave'); }} catch(err) {{}}",
                "        }}",
                "    }}",
                "}});",
                "document.addEventListener('drop', function(e) {{",
                "    dragCounter = 0;",
                "    hideCaret();",
                "    let dropInfo = currentDropInfo || lastValidDropInfo;",
                "    currentDropInfo = null;",
                "    let dropRange = currentDropRange || lastValidDropRange;",
                "    currentDropRange = null;",
                "    if (!dropInfo && !dropRange && e.clientX > 0 && e.clientY > 0) {{",
                "        updateCaretToPoint(e.clientX, e.clientY);",
                "        dropInfo = currentDropInfo || lastValidDropInfo;",
                "        currentDropInfo = null;",
                "        dropRange = currentDropRange || lastValidDropRange;",
                "        currentDropRange = null;",
                "    }}",
                "    hideCaret();",
                "    const sig = getSignatureElement();",
                "    if (isInternalDrag && draggedElement) {{",
                "        e.preventDefault();",
                "        try {{",
                "            resetDraggedElement();",
                "            const oldParent = draggedElement.parentElement;",
                "            if (dropInfo && dropInfo.isTop) {{",
                "                const blocks = getContentBlocks(draggedElement);",
                "                const firstBlock = blocks.length > 0 ? blocks[0] : null;",
                "                if (firstBlock && isBlockEmpty(firstBlock) && firstBlock !== sig) {{",
                "                    firstBlock.innerHTML = '';",
                "                    firstBlock.appendChild(draggedElement);",
                "                }} else {{",
                "                    let topElem = draggedElement;",
                "                    if (!topElem || topElem.nodeName !== 'P') {{",
                "                        const newP = document.createElement('p');",
                "                        newP.style.margin = '4px 0';",
                "                        newP.appendChild(draggedElement);",
                "                        topElem = newP;",
                "                    }}",
                "                    if (firstBlock) {{",
                "                        document.body.insertBefore(topElem, firstBlock);",
                "                    }} else if (sig) {{",
                "                        document.body.insertBefore(topElem, sig);",
                "                    }} else {{",
                "                        document.body.appendChild(topElem);",
                "                    }}",
                "                }}",
                "                if (oldParent && oldParent !== document.body && oldParent !== draggedElement.parentElement && isBlockEmpty(oldParent)) {{",
                "                    oldParent.remove();",
                "                }}",
                "                const allBlocks = getContentBlocks();",
                "                for (let i = 0; i < allBlocks.length; i++) {{",
                "                    const b = allBlocks[i];",
                "                    if (b === sig || b.contains(draggedElement)) break;",
                "                    if (isBlockEmpty(b)) {{",
                "                        b.remove();",
                "                    }} else {{",
                "                        break;",
                "                    }}",
                "                }}",
                "            }} else if (dropRange) {{",
                "                dropRange = constrainRangeBeforeSignature(dropRange);",
                "                let container = dropRange.startContainer;",
                "                let block = (container.nodeType === 1) ? container : container.parentElement;",
                "                block = block ? block.closest('p, div, li, blockquote') : null;",
                "                if (container === document.body) {{",
                "                    const childAtOffset = document.body.childNodes[dropRange.startOffset];",
                "                    const prevChild = dropRange.startOffset > 0 ? document.body.childNodes[dropRange.startOffset - 1] : null;",
                "                    if (prevChild && isBlockEmpty(prevChild) && prevChild !== sig) {{",
                "                        block = prevChild;",
                "                    }} else if (childAtOffset && isBlockEmpty(childAtOffset) && childAtOffset !== sig) {{",
                "                        block = childAtOffset;",
                "                    }}",
                "                }}",
                "                if (block && isBlockEmpty(block) && block !== sig) {{",
                "                    block.innerHTML = '';",
                "                    block.appendChild(draggedElement);",
                "                }} else if (container.nodeType === 1 && container.childNodes.length === 1 && container.childNodes[0].nodeName === 'BR') {{",
                "                    container.innerHTML = '';",
                "                    container.appendChild(draggedElement);",
                "                }} else {{",
                "                    let toInsert = draggedElement;",
                "                    if (container === document.body && toInsert.nodeName === 'IMG') {{",
                "                        const newP = document.createElement('p');",
                "                        newP.style.margin = '4px 0';",
                "                        newP.appendChild(toInsert);",
                "                        toInsert = newP;",
                "                    }}",
                "                    dropRange.insertNode(toInsert);",
                "                }}",
                "                if (oldParent && oldParent !== document.body && oldParent !== draggedElement.parentElement && isBlockEmpty(oldParent)) {{",
                "                    oldParent.remove();",
                "                }}",
                "            }} else if (sig) {{",
                "                let fallbackElem = draggedElement;",
                "                if (!fallbackElem || fallbackElem.nodeName !== 'P') {{",
                "                    const newP = document.createElement('p');",
                "                    newP.style.margin = '4px 0';",
                "                    newP.appendChild(draggedElement);",
                "                    fallbackElem = newP;",
                "                }}",
                "                document.body.insertBefore(fallbackElem, sig);",
                "                if (oldParent && oldParent !== document.body && oldParent !== draggedElement.parentElement && isBlockEmpty(oldParent)) {{",
                "                    oldParent.remove();",
                "                }}",
                "            }} else {{",
                "                let fallbackElem = draggedElement;",
                "                if (!fallbackElem || fallbackElem.nodeName !== 'P') {{",
                "                    const newP = document.createElement('p');",
                "                    newP.style.margin = '4px 0';",
                "                    newP.appendChild(draggedElement);",
                "                    fallbackElem = newP;",
                "                }}",
                "                document.body.appendChild(fallbackElem);",
                "                if (oldParent && oldParent !== document.body && oldParent !== draggedElement.parentElement && isBlockEmpty(oldParent)) {{",
                "                    oldParent.remove();",
                "                }}",
                "            }}",
                "            const sel = window.getSelection();",
                "            if (sel) {{",
                "                const afterRange = document.createRange();",
                "                afterRange.setStartAfter(draggedElement);",
                "                afterRange.collapse(true);",
                "                sel.removeAllRanges();",
                "                sel.addRange(afterRange);",
                "            }}",
                "        }} catch(err) {{",
                "            console.error('Drop error:', err);",
                "        }} finally {{",
                "            resetDraggedElement();",
                "            if (isInternalDrag) {{",
                "                try {{ window.webkit.messageHandlers.meridianDrag.postMessage('internal_end'); }} catch(err) {{}}",
                "            }}",
                "            isInternalDrag = false;",
                "            draggedElement = null;",
                "            currentDropInfo = null;",
                "            currentDropRange = null;",
                "            lastValidDropInfo = null;",
                "            lastValidDropRange = null;",
                "            hideCaret();",
                "        }}",
                "        return;",
                "    }}",
                "    if (isInternalDrag) {{",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('internal_end'); }} catch(err) {{}}",
                "    }}",
                "    resetDraggedElement();",
                "    isInternalDrag = false;",
                "    draggedElement = null;",
                "    try {{ window.webkit.messageHandlers.meridianDrag.postMessage('drop'); }} catch(err) {{}}",
                "    e.preventDefault();",
                "    if (dropInfo && dropInfo.isTop) {{",
                "        window.__lastDropWasTop = true;",
                "    }} else {{",
                "        window.__lastDropWasTop = false;",
                "    }}",
                "    if (dropRange) {{",
                "        dropRange = constrainRangeBeforeSignature(dropRange);",
                "        try {{",
                "            const sel = window.getSelection();",
                "            sel.removeAllRanges();",
                "            sel.addRange(dropRange);",
                "        }} catch(err) {{}}",
                "    }}",
                "}});",
                "function handleEscape(e) {{",
                "    if (e.key === 'Escape') {{",
                "        handleDragEnd();",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('cancel'); }} catch(err) {{}}",
                "    }}",
                "}};",
                "window.addEventListener('keydown', handleEscape);",
                "document.addEventListener('keydown', handleEscape);",
                "window.addEventListener('mouseleave', function(e) {{ hideCaret(); }});",
                "document.addEventListener('mouseleave', function(e) {{ hideCaret(); }});",
                "window.addEventListener('scroll', function() {{ hideCaret(); }}, true);",
                "</script></head><body contenteditable='true'><p><br></p></body></html>"
            ),
            font_family, font_size
        );

        let editor_weak_for_policy = self.downgrade();
        web_view.connect_decide_policy(move |_, decision, decision_type| {
            if decision_type == webkit6::PolicyDecisionType::NavigationAction
                || decision_type == webkit6::PolicyDecisionType::NewWindowAction
            {
                if let Ok(nav) = decision
                    .clone()
                    .downcast::<webkit6::NavigationPolicyDecision>()
                {
                    if let Some(mut action) = nav.navigation_action() {
                        if let Some(req) = action.request() {
                            if let Some(uri) = req.uri() {
                                if uri.starts_with("http://")
                                    || uri.starts_with("https://")
                                    || uri.starts_with("mailto:")
                                {
                                    let _ = gio::AppInfo::launch_default_for_uri(
                                        &uri,
                                        None::<&gio::AppLaunchContext>,
                                    );
                                    decision.ignore();
                                    return true;
                                } else if uri.starts_with("file://") {
                                    let file = gio::File::for_uri(&uri);
                                    if let Some(path) = file.path() {
                                        if path.is_file() {
                                            if let Some(editor) = editor_weak_for_policy.upgrade() {
                                                editor.dispatch_files_dropped(vec![path]);
                                            }
                                        }
                                    }
                                    decision.ignore();
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            false
        });

        let drop_target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop_target.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);

        drop_target.connect_enter(move |_, _, _| {
            gdk::DragAction::COPY
        });

        let web_view_for_leave = web_view.clone();
        drop_target.connect_leave(move |_| {
            web_view_for_leave.evaluate_javascript(
                "hideCaret(); currentDropRange = null; currentDropInfo = null;",
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
        });

        let editor_weak_for_drop = self.downgrade();
        let web_view_for_drop_eval = web_view.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            web_view_for_drop_eval.evaluate_javascript(
                "hideCaret(); currentDropRange = null;",
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(editor) = editor_weak_for_drop.upgrade() else {
                    return false;
                };
                let mut paths = Vec::new();
                if let Ok(file_list) = value.get::<gdk::FileList>() {
                    for file in file_list.files() {
                        if let Some(path) = file.path() {
                            if path.is_file() {
                                paths.push(path);
                            }
                        }
                    }
                } else if let Ok(file) = value.get::<gio::File>() {
                    if let Some(path) = file.path() {
                        if path.is_file() {
                            paths.push(path);
                        }
                    }
                }
                if !paths.is_empty() {
                    editor.dispatch_files_dropped(paths);
                    return true;
                }
                false
            }));
            res.unwrap_or(false)
        });
        web_view.add_controller(drop_target);

        *self.imp().web_view.borrow_mut() = Some(web_view.clone());
        let editor_weak = self.downgrade();
        web_view.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.flush_pending_scripts();
                }
            }
        });
        web_view.load_html(&initial_html, None);

        let web_view_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&web_view)
            .hexpand(true)
            .vexpand(true)
            .css_classes(vec!["composer-editor-scrolled"])
            .build();

        vbox.append(&web_view_scroll);
        vbox.set_parent(self);

        // Toolbar action wiring
        btn_bold.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('bold', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_italic.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('italic', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_underline.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('underline', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_strikethrough.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('strikethrough', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_clear.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('removeFormat', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        dropdown_format.connect_selected_notify(glib::clone!(
            #[weak]
            web_view,
            move |dropdown| {
                let tag = match dropdown.selected() {
                    1 => "h1",
                    2 => "h2",
                    3 => "h3",
                    4 => "pre",
                    _ => "p",
                };
                let script = format!("document.execCommand('formatBlock', false, '{tag}');");
                web_view.evaluate_javascript(
                    &script,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        dropdown_font_size.connect_selected_notify(glib::clone!(
            #[weak]
            web_view,
            move |dropdown| {
                let size = match dropdown.selected() {
                    1 => 10,
                    2 => 12,
                    3 => 14,
                    4 => 16,
                    5 => 18,
                    6 => 24,
                    7 => 32,
                    _ => 0,
                };
                let script = if size == 0 {
                    "document.execCommand('styleWithCSS', false, true); document.execCommand('fontSize', false, '3');".to_string()
                } else {
                    format!(
                        concat!(
                            "document.execCommand('styleWithCSS', false, true);",
                            "document.execCommand('fontSize', false, '7');",
                            "(function() {{",
                            "    var spans = document.querySelectorAll('span[style*=\"-webkit-xxx-large\"], font[size=\"7\"]');",
                            "    for (var i = 0; i < spans.length; i++) {{",
                            "        spans[i].removeAttribute('size');",
                            "        spans[i].style.fontSize = '{size}px';",
                            "    }}",
                            "}})();"
                        ),
                        size = size
                    )
                };
                web_view.evaluate_javascript(
                    &script,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_list.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('insertUnorderedList', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_ordered_list.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('insertOrderedList', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_blockquote.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('formatBlock', false, 'blockquote');",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        let entry_url_weak_for_open = entry_url.downgrade();
        let entry_label_weak_for_open = entry_label.downgrade();
        let web_view_for_open = web_view.clone();
        link_popover.connect_notify_local(Some("visible"), move |popover, _| {
            if !popover.is_visible() {
                return;
            }
            let Some(entry_url) = entry_url_weak_for_open.upgrade() else {
                return;
            };
            let Some(entry_label) = entry_label_weak_for_open.upgrade() else {
                return;
            };
            let entry_u = entry_url.clone();
            let entry_l = entry_label.clone();
            web_view_for_open.evaluate_javascript(
                r#"(() => {
    let sel = window.getSelection();
    let text = sel ? sel.toString().trim() : '';
    let href = '';
    if (sel && sel.rangeCount > 0) {
        let node = sel.anchorNode;
        if (node && node.nodeType === 3) node = node.parentElement;
        let a = node ? node.closest('a') : null;
        if (a) {
            href = a.getAttribute('href') || a.href || '';
            if (!text) text = a.textContent ? a.textContent.trim() : '';
        }
    }
    return JSON.stringify({ text, href });
})()"#,
                None,
                None,
                None::<&gio::Cancellable>,
                move |result| {
                    if let Ok(val) = result {
                        let json_str = val.to_str();
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            let text = parsed.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            let href = parsed.get("href").and_then(|v| v.as_str()).unwrap_or("");
                            entry_u.set_text(href);
                            entry_l.set_text(text);
                            if href.is_empty() {
                                entry_u.grab_focus();
                            } else {
                                entry_l.grab_focus();
                            }
                        }
                    }
                },
            );
        });

        let web_view_for_link = web_view.clone();
        let link_popover_weak = link_popover.downgrade();
        let entry_url_weak = entry_url.downgrade();
        let entry_label_weak = entry_label.downgrade();
        btn_insert_link_confirm.connect_clicked(move |_| {
            let Some(entry_url) = entry_url_weak.upgrade() else {
                return;
            };
            let Some(entry_label) = entry_label_weak.upgrade() else {
                return;
            };
            let mut url = entry_url.text().trim().to_string();
            let label = entry_label.text().trim().to_string();
            if !url.is_empty() {
                if !url.contains("://") && !url.starts_with("mailto:") && !url.starts_with('#') {
                    if url.contains('@') && !url.contains('/') {
                        url = format!("mailto:{url}");
                    } else {
                        url = format!("https://{url}");
                    }
                }

                let url_json = serde_json::to_string(&url).unwrap_or_else(|_| format!("\"{}\"", url));
                let label_json = serde_json::to_string(&label).unwrap_or_else(|_| format!("\"{}\"", label));

                let js = format!(
                    r#"(() => {{
    const url = {url_json};
    const label = {label_json};
    const sel = window.getSelection();
    let existingA = null;
    if (sel && sel.rangeCount > 0) {{
        let node = sel.anchorNode;
        if (node && node.nodeType === 3) node = node.parentElement;
        existingA = node ? node.closest('a') : null;
    }}
    if (existingA) {{
        existingA.href = url;
        if (label) existingA.textContent = label;
    }} else if (label) {{
        const a = document.createElement('a');
        a.href = url;
        a.textContent = label;
        if (sel && sel.rangeCount > 0) {{
            const range = sel.getRangeAt(0);
            range.deleteContents();
            range.insertNode(a);
            range.setStartAfter(a);
            range.setEndAfter(a);
            sel.removeAllRanges();
            sel.addRange(range);
        }} else {{
            document.body.appendChild(a);
        }}
    }} else {{
        if (sel && !sel.isCollapsed) {{
            document.execCommand('createLink', false, url);
        }} else {{
            const a = document.createElement('a');
            a.href = url;
            a.textContent = url;
            if (sel && sel.rangeCount > 0) {{
                const range = sel.getRangeAt(0);
                range.deleteContents();
                range.insertNode(a);
                range.setStartAfter(a);
                range.setEndAfter(a);
                sel.removeAllRanges();
                sel.addRange(range);
            }} else {{
                document.body.appendChild(a);
            }}
        }}
    }}
}})();"#
                );

                web_view_for_link.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
                entry_url.set_text("");
                entry_label.set_text("");
            }
            if let Some(popover) = link_popover_weak.upgrade() {
                popover.popdown();
            }
        });

        btn_rule.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('insertHorizontalRule', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        let editor_for_image = self.downgrade();
        btn_image.connect_clicked(move |_| {
            let Some(editor) = editor_for_image.upgrade() else {
                return;
            };
            let parent = editor.root().and_downcast::<gtk::Window>();
            let file_dialog = gtk::FileDialog::builder()
                .title(&gettext("Select image to insert"))
                .modal(true)
                .build();
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&gettext("Images (*.png, *.jpg, *.jpeg, *.gif, *.svg, *.webp)")));
            filter.add_mime_type("image/png");
            filter.add_mime_type("image/jpeg");
            filter.add_mime_type("image/gif");
            filter.add_mime_type("image/svg+xml");
            filter.add_mime_type("image/webp");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            file_dialog.set_filters(Some(&filters));

            let editor_weak = editor_for_image.clone();
            file_dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        if let Some(editor) = editor_weak.upgrade() {
                            editor.insert_image_from_path(&path);
                        }
                    }
                }
            });
        });

        btn_undo.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('undo', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_redo.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "document.execCommand('redo', false, null);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));
    }
}

impl Default for HtmlEditor {
    fn default() -> Self {
        Self::new()
    }
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(
            TABLE[(((first & 0x03) << 4) | second.map_or(0, |value| value >> 4)) as usize] as char,
        );
        encoded.push(second.map_or('=', |value| {
            TABLE[(((value & 0x0f) << 2) | third.map_or(0, |third| third >> 6)) as usize] as char
        }));
        encoded.push(third.map_or('=', |value| TABLE[(value & 0x3f) as usize] as char));
    }

    encoded
}

#[derive(Debug, serde::Deserialize)]
struct ImageMenuPayload {
    id: String,
    x: i32,
    y: i32,
    #[allow(dead_code)]
    img_x: i32,
    #[allow(dead_code)]
    img_y: i32,
    #[allow(dead_code)]
    img_w: i32,
    #[allow(dead_code)]
    img_h: i32,
    align: String,
    size: String,
}

#[derive(Debug, serde::Deserialize)]
struct ConvertedImageData {
    src: String,
    #[serde(default)]
    alt: String,
}

pub fn parse_image_data_uri_or_path(src: &str, alt: &str) -> Option<(Vec<u8>, String, String)> {
    if let Some(rest) = src.strip_prefix("data:") {
        let mut parts = rest.splitn(2, ',');
        let header = parts.next()?;
        let body = parts.next()?;
        let mime = header.split(';').next().unwrap_or("image/png").trim();
        let is_base64 = header.contains(";base64");
        let bytes = if is_base64 {
            glib::base64_decode(body.trim())
        } else {
            body.as_bytes().to_vec()
        };
        if bytes.is_empty() {
            return None;
        }
        let ext = match mime {
            "image/png" => "png",
            "image/jpeg" | "image/jpg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            _ => "png",
        };
        let alt_clean = alt.trim();
        let filename = if !alt_clean.is_empty() && alt_clean.contains('.') {
            alt_clean.to_string()
        } else if !alt_clean.is_empty() {
            format!("{alt_clean}.{ext}")
        } else {
            format!("bild_{}.{ext}", &glib::uuid_string_random()[..6])
        };
        Some((bytes, mime.to_string(), filename))
    } else if let Some(_path_str) = src.strip_prefix("file://") {
        let file = gio::File::for_uri(src);
        let path = file.path()?;
        let bytes = std::fs::read(&path).ok()?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            _ => "application/octet-stream",
        };
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("bild.png")
            .to_string();
        Some((bytes, mime.to_string(), filename))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_image_data_uri_png() {
        let dummy_png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let data_uri = format!("data:image/png;base64,{dummy_png_b64}");
        let result = parse_image_data_uri_or_path(&data_uri, "my_screenshot.png");
        assert!(result.is_some());
        let (bytes, mime, filename) = result.unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(filename, "my_screenshot.png");
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x89);
        assert_eq!(bytes[1], 0x50); // 'P'
        assert_eq!(bytes[2], 0x4e); // 'N'
        assert_eq!(bytes[3], 0x47); // 'G'
    }

    #[test]
    fn test_parse_image_data_uri_custom_alt() {
        let dummy_b64 = "dGVzdA=="; // "test"
        let data_uri = format!("data:image/jpeg;base64,{dummy_b64}");
        let result = parse_image_data_uri_or_path(&data_uri, "photo");
        assert!(result.is_some());
        let (bytes, mime, filename) = result.unwrap();
        assert_eq!(mime, "image/jpeg");
        assert_eq!(filename, "photo.jpg");
        assert_eq!(bytes, b"test");
    }

    #[test]
    fn test_parse_image_data_uri_invalid() {
        assert!(parse_image_data_uri_or_path("invalid:not-a-data-uri", "").is_none());
        assert!(parse_image_data_uri_or_path("data:image/png;base64,", "").is_none());
    }

    #[test]
    fn test_image_menu_payload_deserialize() {
        let json = r#"{"id":"__meridian_img_123","x":150,"y":280,"img_x":100,"img_y":200,"img_w":400,"img_h":300,"align":"center","size":"medium"}"#;
        let payload: Result<ImageMenuPayload, _> = serde_json::from_str(json);
        assert!(payload.is_ok());
        let p = payload.unwrap();
        assert_eq!(p.id, "__meridian_img_123");
        assert_eq!(p.x, 150);
        assert_eq!(p.y, 280);
        assert_eq!(p.align, "center");
        assert_eq!(p.size, "medium");
    }

    #[test]
    fn test_image_menu_payload_quoted_deserialize() {
        let quoted_json = r#""{\"id\":\"__meridian_img_999\",\"x\":200,\"y\":350,\"img_x\":180,\"img_y\":300,\"img_w\":500,\"img_h\":400,\"align\":\"left\",\"size\":\"large\"}""#;
        let unquoted: String = serde_json::from_str(quoted_json).unwrap_or_else(|_| quoted_json.to_string());
        let payload: Result<ImageMenuPayload, _> = serde_json::from_str::<ImageMenuPayload>(&unquoted)
            .or_else(|_| serde_json::from_str::<ImageMenuPayload>(quoted_json));
        assert!(payload.is_ok());
        let p = payload.unwrap();
        assert_eq!(p.id, "__meridian_img_999");
        assert_eq!(p.x, 200);
        assert_eq!(p.y, 350);
        assert_eq!(p.align, "left");
        assert_eq!(p.size, "large");
    }

    #[test]
    fn test_prepare_image_data_uri() {
        let tmp = std::env::temp_dir().join("meridian_test_inline_prep.png");
        let png_bytes = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92,
            0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let _ = std::fs::write(&tmp, &png_bytes);
        let res = HtmlEditor::prepare_image_data_uri(&tmp);
        assert!(res.is_some());
        let (data_uri, mime) = res.unwrap();
        assert_eq!(mime, "image/png");
        assert!(data_uri.starts_with("data:image/png;base64,"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_converted_image_data_deserialize() {
        let raw_json = r#"{"src":"data:image/png;base64,dGVzdA==","alt":"test.png"}"#;
        let data: Result<ConvertedImageData, _> = serde_json::from_str(raw_json);
        assert!(data.is_ok());
        let d = data.unwrap();
        assert_eq!(d.src, "data:image/png;base64,dGVzdA==");
        assert_eq!(d.alt, "test.png");

        let quoted_json = r#""{\"src\":\"data:image/png;base64,dGVzdA==\",\"alt\":\"test.png\"}""#;
        let unquoted: String = serde_json::from_str(quoted_json).unwrap_or_else(|_| quoted_json.to_string());
        let data2: Result<ConvertedImageData, _> = serde_json::from_str(&unquoted);
        assert!(data2.is_ok());
        let d2 = data2.unwrap();
        assert_eq!(d2.src, "data:image/png;base64,dGVzdA==");
        assert_eq!(d2.alt, "test.png");
    }

    #[test]
    fn test_is_supported_image() {
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("photo.png")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("photo.JPG")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("photo.jpeg")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("animation.gif")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("vector.svg")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("modern.webp")));
        assert!(!HtmlEditor::is_supported_image(std::path::Path::new("doc.pdf")));
        assert!(!HtmlEditor::is_supported_image(std::path::Path::new("archive.zip")));
        assert!(!HtmlEditor::is_supported_image(std::path::Path::new("text.txt")));
    }
}

