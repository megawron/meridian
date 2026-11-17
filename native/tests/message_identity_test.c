/* SPDX-License-Identifier: GPL-3.0-or-later */

#include <camel/camel.h>
#include <gio/gio.h>
#include <glib.h>

#define __LIBEDATASERVER_H_INSIDE__
#include <libedataserver/e-source.h>
#include <libedataserver/e-source-goa.h>
#include <libedataserver/e-source-mail-identity.h>
#undef __LIBEDATASERVER_H_INSIDE__

#include <string.h>

#include "../eds_camel_message_identity.h"

static ESource *
make_identity_source (const gchar *uid,
                      const gchar *name,
                      const gchar *address)
{
    GError *error = NULL;
    ESource *source = e_source_new_with_uid (uid, NULL, &error);
    g_assert_no_error (error);
    g_assert_nonnull (source);

    ESourceMailIdentity *ext = (ESourceMailIdentity *) e_source_get_extension (
        source, E_SOURCE_EXTENSION_MAIL_IDENTITY);
    g_assert_nonnull (ext);
    if (name)
        e_source_mail_identity_set_name (ext, name);
    if (address)
        e_source_mail_identity_set_address (ext, address);

    return source;
}

static void
test_preserves_shared_mailbox_from_header (void)
{
    const gchar *raw =
        "From: Department Team <shared@example.com>\r\n"
        "To: Customer <customer@example.org>\r\n"
        "Subject: Shared Mailbox Test\r\n"
        "MIME-Version: 1.0\r\n"
        "Content-Type: text/plain; charset=UTF-8\r\n"
        "\r\n"
        "Sent from the shared mailbox.\r\n";

    GError *error = NULL;
    CamelMimeMessage *message = parse_raw_message (
        (const guint8 *) raw, strlen (raw), NULL, &error);
    g_assert_no_error (error);
    g_assert_nonnull (message);

    /* Host account belongs to the personal user, not the shared mailbox. */
    ESource *host_identity = make_identity_source (
        "host-identity-uid", "Personal User", "user@example.com");
    CamelInternetAddress *envelope_from = NULL;

    gboolean ok = resolve_message_sender (
        message, host_identity, host_identity, "host-mail-uid",
        &envelope_from, &error);
    g_assert_true (ok);
    g_assert_no_error (error);
    g_assert_nonnull (envelope_from);

    const gchar *env_name = NULL;
    const gchar *env_email = NULL;
    g_assert_true (camel_internet_address_get (
        envelope_from, 0, &env_name, &env_email));
    g_assert_cmpstr (env_name, ==, "Department Team");
    g_assert_cmpstr (env_email, ==, "shared@example.com");

    CamelInternetAddress *mime_from = camel_mime_message_get_from (message);
    g_assert_nonnull (mime_from);
    const gchar *mime_name = NULL;
    const gchar *mime_email = NULL;
    g_assert_true (camel_internet_address_get (
        mime_from, 0, &mime_name, &mime_email));
    g_assert_cmpstr (mime_name, ==, "Department Team");
    g_assert_cmpstr (mime_email, ==, "shared@example.com");
    g_assert_cmpstr (camel_mime_message_get_source (message), ==, "host-mail-uid");

    g_object_unref (envelope_from);
    g_object_unref (host_identity);
    g_object_unref (message);
}

static void
test_preserves_shared_mailbox_from_on_draft_save (void)
{
    const gchar *raw =
        "From: Support Desk <support@example.com>\r\n"
        "To: Customer <customer@example.org>\r\n"
        "Subject: Draft Test\r\n"
        "\r\n"
        "Draft body.\r\n";

    GError *error = NULL;
    CamelMimeMessage *message = parse_raw_message (
        (const guint8 *) raw, strlen (raw), NULL, &error);
    g_assert_no_error (error);
    g_assert_nonnull (message);

    ESource *host_identity = make_identity_source (
        "host-identity-uid", "Personal User", "user@example.com");

    gboolean ok = set_message_identity (
        message, host_identity, host_identity, "host-mail-uid", &error);
    g_assert_true (ok);
    g_assert_no_error (error);

    CamelInternetAddress *mime_from = camel_mime_message_get_from (message);
    const gchar *mime_name = NULL;
    const gchar *mime_email = NULL;
    g_assert_true (camel_internet_address_get (
        mime_from, 0, &mime_name, &mime_email));
    g_assert_cmpstr (mime_name, ==, "Support Desk");
    g_assert_cmpstr (mime_email, ==, "support@example.com");

    g_object_unref (host_identity);
    g_object_unref (message);
}

static void
test_falls_back_to_host_identity_when_from_missing (void)
{
    const gchar *raw =
        "To: Colleague <colleague@example.org>\r\n"
        "Subject: Fallback Sender\r\n"
        "\r\n"
        "No From header in input.\r\n";

    GError *error = NULL;
    CamelMimeMessage *message = parse_raw_message (
        (const guint8 *) raw, strlen (raw), NULL, &error);
    g_assert_no_error (error);
    g_assert_nonnull (message);

    ESource *host_identity = make_identity_source (
        "host-identity-uid", "Personal User", "user@example.com");
    CamelInternetAddress *envelope_from = NULL;

    gboolean ok = resolve_message_sender (
        message, host_identity, host_identity, "host-mail-uid",
        &envelope_from, &error);
    g_assert_true (ok);
    g_assert_no_error (error);
    g_assert_nonnull (envelope_from);

    const gchar *env_name = NULL;
    const gchar *env_email = NULL;
    g_assert_true (camel_internet_address_get (
        envelope_from, 0, &env_name, &env_email));
    g_assert_cmpstr (env_name, ==, "Personal User");
    g_assert_cmpstr (env_email, ==, "user@example.com");

    CamelInternetAddress *mime_from = camel_mime_message_get_from (message);
    const gchar *mime_name = NULL;
    const gchar *mime_email = NULL;
    g_assert_true (camel_internet_address_get (
        mime_from, 0, &mime_name, &mime_email));
    g_assert_cmpstr (mime_name, ==, "Personal User");
    g_assert_cmpstr (mime_email, ==, "user@example.com");

    g_object_unref (envelope_from);
    g_object_unref (host_identity);
    g_object_unref (message);
}

static void
test_errors_when_both_message_and_identity_lack_sender (void)
{
    const gchar *raw =
        "To: Colleague <colleague@example.org>\r\n"
        "Subject: Missing Sender\r\n"
        "\r\n"
        "Body.\r\n";

    GError *error = NULL;
    CamelMimeMessage *message = parse_raw_message (
        (const guint8 *) raw, strlen (raw), NULL, &error);
    g_assert_no_error (error);
    g_assert_nonnull (message);

    ESource *empty_identity = make_identity_source (
        "empty-identity-uid", "No Address", NULL);
    CamelInternetAddress *envelope_from = NULL;

    gboolean ok = resolve_message_sender (
        message, empty_identity, empty_identity, "host-mail-uid",
        &envelope_from, &error);
    g_assert_false (ok);
    g_assert_null (envelope_from);
    g_assert_error (error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT);

    g_clear_error (&error);
    g_object_unref (empty_identity);
    g_object_unref (message);
}

int
main (int argc, char **argv)
{
    camel_init (NULL, FALSE);
    g_test_init (&argc, &argv, NULL);

    g_test_add_func (
        "/camel-bridge/message-identity/preserves-shared-mailbox-from",
        test_preserves_shared_mailbox_from_header);
    g_test_add_func (
        "/camel-bridge/message-identity/preserves-shared-from-on-draft-save",
        test_preserves_shared_mailbox_from_on_draft_save);
    g_test_add_func (
        "/camel-bridge/message-identity/falls-back-to-host-identity",
        test_falls_back_to_host_identity_when_from_missing);
    g_test_add_func (
        "/camel-bridge/message-identity/errors-without-any-sender",
        test_errors_when_both_message_and_identity_lack_sender);

    return g_test_run ();
}
