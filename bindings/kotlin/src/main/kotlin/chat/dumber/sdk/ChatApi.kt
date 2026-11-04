package chat.dumber.sdk

/** Blocking operations. Call from a worker thread. Policy and network behavior live in Rust. */
interface ChatApi {
    fun requestCode(phone: String): CodeRequest
    fun verify(phone: String, code: String): Session
    fun sessionStatus(): SessionStatus
    fun reviewPairing(code: String): PairingReview
    fun approvePairing(code: String): PairingReview
    fun recipientStatus(phone: String): RecipientStatus
    fun checkRecipient(phone: String, force: Boolean = false): RecipientStatus
    fun openChat(phone: String, cachedOnly: Boolean = false): Conversation
    fun conversations(): List<Conversation>
    fun conversationInfo(conversationId: String): Conversation
    fun createGroup(name: String, phones: List<String>, operationId: String): Conversation
    fun inviteMember(conversationId: String, phone: String)
    fun renameGroup(conversationId: String, name: String)
    fun removeMember(conversationId: String, contactId: String)
    fun grantAdmin(conversationId: String, contactId: String)
    fun join(conversationId: String)
    fun leave(conversationId: String)
    fun sendText(conversationId: String, body: String, transactionId: String): SentMessage
    fun sync(): SyncUpdate
    fun devices(phone: String? = null): DeviceList
    fun exportKeys(path: String, passphrase: String)
    fun importKeys(path: String, passphrase: String): KeyImport
    fun logout()
}

internal object NativeBridge {
    init { System.loadLibrary("dumber_chat_core") }
    @JvmStatic external fun invoke(request: String): String
}
