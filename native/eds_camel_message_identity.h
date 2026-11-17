/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef MERIDIAN_EDS_CAMEL_MESSAGE_IDENTITY_H
#define MERIDIAN_EDS_CAMEL_MESSAGE_IDENTITY_H

static void
parse_email_address_parts (const gchar *input_address,
                           gchar **out_name,
                           gchar **out_email)
{
    if (out_name)
        *out_name = NULL;
    if (out_email)
        *out_email = NULL;

    if (!input_address || !*input_address)
        return;

    gchar *trimmed = g_strdup (input_address);
    g_strstrip (trimmed);

    gchar *angle_start = strchr (trimmed, '<');
    gchar *angle_end = strrchr (trimmed, '>');

    if (angle_start && angle_end && angle_end > angle_start) {
        *angle_end = '\0';
        gchar *email_part = g_strdup (angle_start + 1);
        g_strstrip (email_part);

        *angle_start = '\0';
        gchar *name_part = g_strdup (trimmed);
        g_strstrip (name_part);
        gsize name_len = strlen (name_part);
        if (name_len >= 2 && name_part[0] == '"' && name_part[name_len - 1] == '"') {
            name_part[name_len - 1] = '\0';
            memmove (name_part, name_part + 1, name_len - 1);
            g_strstrip (name_part);
        }

        if (out_email)
            *out_email = email_part;
        else
            g_free (email_part);
        if (out_name && *name_part)
            *out_name = name_part;
        else
            g_free (name_part);
    } else {
        if (out_email)
            *out_email = g_strdup (trimmed);
    }

    g_free (trimmed);
}

static gboolean
set_camel_internet_address (CamelInternetAddress *cia,
                            const gchar *provided_name,
                            const gchar *provided_address)
{
    if (!cia || !provided_address || !*provided_address)
        return FALSE;

    gchar *parsed_name = NULL;
    gchar *parsed_email = NULL;

    parse_email_address_parts (provided_address, &parsed_name, &parsed_email);

    const gchar *final_email = (parsed_email && *parsed_email) ? parsed_email : provided_address;
    const gchar *final_name = (provided_name && *provided_name)
        ? provided_name
        : (parsed_name && *parsed_name ? parsed_name : NULL);

    gint index = camel_internet_address_add (cia, final_name, final_email);
    gboolean success = (index >= 0);

    g_free (parsed_name);
    g_free (parsed_email);
    return success;
}

/* Extract a validated copy of the message's existing From header when it
 * already carries at least one internet address.  Upstream Evolution
 * (e-mail-session-utils.c / mail-ops.c) preserves the MIME From header set by
 * the composer and forwards that address to camel_transport_send_to_sync,
 * which allows shared/delegate mailboxes and aliases to send under the chosen
 * From address while authenticating through the host account's transport. */
static CamelInternetAddress *
extract_valid_message_from (CamelMimeMessage *message)
{
    CamelInternetAddress *existing_from;
    CamelInternetAddress *copy;
    gint len;
    gint i;

    if (!message)
        return NULL;

    existing_from = camel_mime_message_get_from (message);
    if (!existing_from)
        return NULL;

    len = camel_address_length (CAMEL_ADDRESS (existing_from));
    if (len <= 0)
        return NULL;

    copy = camel_internet_address_new ();
    for (i = 0; i < len; i++) {
        const gchar *addr_name = NULL;
        const gchar *addr_email = NULL;

        if (camel_internet_address_get (existing_from, i, &addr_name, &addr_email) &&
            addr_email && *addr_email && strchr (addr_email, '@')) {
            camel_internet_address_add (
                copy,
                (addr_name && *addr_name) ? addr_name : NULL,
                addr_email);
        }
    }

    if (camel_address_length (CAMEL_ADDRESS (copy)) <= 0) {
        g_object_unref (copy);
        return NULL;
    }

    return copy;
}

static gboolean
resolve_message_sender (CamelMimeMessage *message,
                        ESource *identity_source,
                        ESource *account_source,
                        const gchar *source_uid,
                        CamelInternetAddress **out_from,
                        GError **error)
{
    ESourceMailIdentity *identity_extension = NULL;
    CamelInternetAddress *from = NULL;
    gchar *address = NULL;
    gchar *name = NULL;
    gboolean success = FALSE;

    if (out_from)
        *out_from = NULL;

    if (!message) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "A MIME message is required to resolve the sender address");
        return FALSE;
    }

    from = extract_valid_message_from (message);
    if (from) {
        if (source_uid && *source_uid)
            camel_mime_message_set_source (message, source_uid);
        if (out_from)
            *out_from = g_steal_pointer (&from);
        success = TRUE;
        goto finish;
    }

    if (identity_source &&
        e_source_has_extension (identity_source, E_SOURCE_EXTENSION_MAIL_IDENTITY)) {
        identity_extension = (ESourceMailIdentity *) e_source_get_extension (
            identity_source, E_SOURCE_EXTENSION_MAIL_IDENTITY);
    }
    address = identity_extension
        ? e_source_mail_identity_dup_address (identity_extension)
        : NULL;
    name = identity_extension
        ? e_source_mail_identity_dup_name (identity_extension)
        : NULL;

    if (!address || !*address) {
        g_clear_pointer (&address, g_free);
        if (identity_source && e_source_has_extension (identity_source, "GNOME Online Accounts")) {
            ESourceExtension *goa = e_source_get_extension (identity_source, "GNOME Online Accounts");
            address = e_source_goa_dup_address (E_SOURCE_GOA (goa));
        } else if (account_source && e_source_has_extension (account_source, "GNOME Online Accounts")) {
            ESourceExtension *goa = e_source_get_extension (account_source, "GNOME Online Accounts");
            address = e_source_goa_dup_address (E_SOURCE_GOA (goa));
        }
    }

    if (!address || !*address) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "The selected EDS mail identity has no sender address");
        goto finish;
    }

    from = camel_internet_address_new ();
    if (!set_camel_internet_address (
            from, name && *name ? name : NULL, address)) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "The EDS sender address could not be parsed as an internet address");
        goto finish;
    }

    camel_medium_remove_header (CAMEL_MEDIUM (message), "From");
    camel_mime_message_set_from (message, from);
    if (source_uid && *source_uid)
        camel_mime_message_set_source (message, source_uid);
    if (out_from)
        *out_from = g_steal_pointer (&from);
    success = TRUE;

finish:
    g_clear_object (&from);
    g_free (address);
    g_free (name);
    return success;
}

static gboolean
set_message_identity (CamelMimeMessage *message,
                      ESource *identity_source,
                      ESource *account_source,
                      const gchar *source_uid,
                      GError **error)
{
    return resolve_message_sender (
        message, identity_source, account_source, source_uid, NULL, error);
}

static CamelMimeMessage *
parse_raw_message (const guint8 *raw_message,
                   gsize raw_message_len,
                   GCancellable *cancellable,
                   GError **error)
{
    CamelMimeMessage *message;
    CamelStream *stream;

    message = camel_mime_message_new ();
    stream = camel_stream_mem_new_with_buffer (
        (const gchar *) raw_message, raw_message_len);
    if (!camel_data_wrapper_construct_from_stream_sync (
            CAMEL_DATA_WRAPPER (message), stream, cancellable, error)) {
        g_clear_object (&stream);
        g_clear_object (&message);
        return NULL;
    }

    g_clear_object (&stream);
    return message;
}

#endif /* MERIDIAN_EDS_CAMEL_MESSAGE_IDENTITY_H */
