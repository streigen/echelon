<script lang="ts">
    import "$lib/styles/chat.css";

    // ── Layout state ──────────────────────────────────────────
    let sidebarCollapsed       = $state(false);
    let participantsPanelVisible = $state(true);

    // ── Room / category state ─────────────────────────────────
    // `collapsed` tracks which category headers are folded in the room list
    let collapsed: Record<string, boolean> = $state({});
    function toggleCategory(name: string) { collapsed[name] = !collapsed[name]; }

    // `collapsedGroups` tracks which participant groups are folded in the right panel
    let collapsedGroups: Record<string, boolean> = $state({});
    function toggleGroup(name: string) { collapsedGroups[name] = !collapsedGroups[name]; }

    // ── Voice / audio state ───────────────────────────────────
    let muted          = $state(false);
    let deafened       = $state(false);
    let activeVoiceRoom = $state<string | null>(null);

    function joinVoiceRoom(name: string) {
        // Toggle: joining an already-active room leaves it
        activeVoiceRoom = activeVoiceRoom === name ? null : name;
    }

    // ── Collapsed-dock hover-stack visibility ─────────────────
    /*
     * The floating action stack above the profile pfp is shown on mouseenter
     * and hidden 2 s after the last mouseleave. The timer is canceled if the
     * cursor re-enters the dock zone (profile OR the stack itself) before it
     * fires. Mouse tracking is handled by a single wrapper div (.spaces-dock-zone)
     * so crossing the invisible gap between the profile and the stack never
     * triggers a spurious hide.
     */
    let profileStackVisible = $state(false);
    let hideTimer: ReturnType<typeof setTimeout> | null = null;

    function showStack() {
        // Guard: only show when the sidebar is actually collapsed
        if (!sidebarCollapsed) return;
        cancelHide();
        profileStackVisible = true;
    }

    function scheduleHide() {
        hideTimer = setTimeout(() => {
            profileStackVisible = false;
            hideTimer = null;
        }, 2000);
    }

    function cancelHide() {
        if (hideTimer) { clearTimeout(hideTimer); hideTimer = null; }
    }

    // Immediately clear the stack whenever the sidebar re-opens so the
    // invisible ::after hit-area bridge doesn't block the collapse button
    $effect(() => {
        if (!sidebarCollapsed) {
            cancelHide();
            profileStackVisible = false;
        }
    });

    // ── Static demo data ──────────────────────────────────────
    const spaceName  = "echelon";
    const homeserver = "matrix.org";
    const username   = "Clumsy ☆";
    const activeRoom = "general";

    const voiceParticipants = ["Clumsy ☆", "Human", "flaxeneel2"];

    const messages = [
        { user: "Clumsy ☆",   time: "12:02 pm", text: "hello world" },
        { user: "flaxeneel2", time: "12:03 pm", text: "hello clumsy",     repliedTo: "Clumsy ☆: hello world" },
        { user: "Clumsy ☆",   time: "12:03 pm", text: "look, cool img:",  image: true },
        { user: "Clumsy ☆",   time: "12:05 pm", text: "very nice image",  repliedTo: "flaxeneel2: hello clumsy" },
    ];

    const participantGroups = [
        { title: "owners",     users: ["flaxeneel2"] },
        { title: "moderators", users: ["helloflaxee:)", "jkhala"] },
        { title: "members",    users: ["clumsy ☆"] },
    ];

    const categories = [
        { name: "rooms", rooms: [
            { name: "announcements",       type: "text",  encrypted: true  },
            { name: "general",             type: "text",  encrypted: true  },
            { name: "very trustworthy",    type: "text",  encrypted: false },
            { name: "very trustworthy x2", type: "text",  encrypted: false },
        ]},
        { name: "cooking", rooms: [
            { name: "cookies", type: "text",  encrypted: false },
            { name: "creme",   type: "text",  encrypted: true  },
        ]},
        { name: "voice rooms", rooms: [
            { name: "voice room", type: "voice", encrypted: false },
        ]},
    ];
</script>

<!-- Root container; .sidebar-collapsed drives all collapsed-state CSS overrides -->
<div class="app-container" class:sidebar-collapsed={sidebarCollapsed}>

    <!-- ── Left panel ── -->
    <div class="left-panel">
    <div class="left-panel-columns">

        <!-- Spaces rail: narrow icon strip on the far left -->
        <nav class="spaces-bar">

            <!-- DM shortcut at the top -->
            <div class="spaces-bar-header section-header-row gapped-line-bottom">
                <div class="spaces-dm-icon-wrapper">
                    <img src="/dm.svg" class="spaces-dm-icon" alt="direct messages" />
                </div>
            </div>

            <!-- Space icons -->
            <div class="spaces-list">
                <div class="space active"><span>ec</span></div>
                <div class="space"><span>sp</span></div>
            </div>

            <!-- Collapse/expand sidebar button (hidden when collapsed) -->
            <div class="spaces-bottom-zone">
                <button
                    class="spaces-collapse-btn"
                    class:rotated={sidebarCollapsed}
                    onclick={() => sidebarCollapsed = !sidebarCollapsed}
                    title="collapse sidebar"
                >
                    <img src="/dropdown.svg" class="spaces-collapse-arrow" alt="collapse" />
                </button>
            </div>

            <!--
                Collapsed dock: shown only when sidebar is collapsed.
                Contains the profile circle + floating action stack.
            -->
            <div class="spaces-collapsed-dock">
                <!--
                    .spaces-dock-zone is the single mouse-tracking boundary.
                    It covers both the profile circle and the floating stack above it,
                    so moving the cursor between them never triggers the hide timer.
                -->
                <div
                    class="spaces-dock-zone"
                    role="presentation"
                    onmouseenter={showStack}
                    onmouseleave={scheduleHide}
                >
                    <!-- Profile circle: hover stack anchor (pfp rendered separately as persistent element) -->
                    <div class="spaces-collapsed-profile" role="group" aria-label="profile actions">

                        <!--
                            Floating action stack. Animates in/out via opacity + transform.
                            .stack-visible is toggled by JS (not CSS :hover) so the 2s
                            linger timer works correctly.
                            The ::after pseudo-element (CSS) creates an invisible bridge
                            over the gap between the stack and the profile circle.
                        -->
                        <div
                            class="spaces-hover-stack"
                            class:stack-visible={profileStackVisible}
                            role="toolbar"
                            tabindex="-1"
                        >
                            <div class="spaces-collapsed-divider"></div>

                            <!-- Re-open sidebar -->
                            <button
                                class="spaces-hover-btn"
                                onclick={() => sidebarCollapsed = false}
                                title="re-open sidebar"
                            >
                                <img src="/dropdown.svg" class="spaces-hover-icon reopen" alt="re-open sidebar" />
                            </button>

                            <!-- Mute toggle -->
                            <button
                                class="spaces-hover-btn"
                                class:active={muted}
                                onclick={() => muted = !muted}
                                title={muted ? "unmute" : "mute"}
                            >
                                <img src={muted ? "/muted.svg" : "/unmuted.svg"} class="spaces-hover-icon" alt="mute" />
                            </button>

                            <!-- Deafen toggle -->
                            <button
                                class="spaces-hover-btn"
                                class:active={deafened}
                                onclick={() => deafened = !deafened}
                                title={deafened ? "undeafen" : "deafen"}
                            >
                                <img src={deafened ? "/deafened.svg" : "/undeafened.svg"} class="spaces-hover-icon flip-h" alt="deafen" />
                            </button>
                        </div>
                    </div>
                </div>
            </div>

        </nav>

        <!-- Channel sidebar shell: animates layout width only on lightweight wrapper -->
        <div class="sidebar-shell">

        <!-- Channel sidebar: room list + space header (animates with transform/opacity) -->
        <aside class="sidebar-container">

            <header class="space-nameinfo section-header-row gapped-line-bottom">
                <div class="space-nameinfo-text">
                    <span class="space-title">{spaceName}</span>
                    <span class="space-homeserver">{homeserver}</span>
                </div>
                <img src="/dropdown.svg" class="space-dropdown-arrow" alt="options" />
            </header>

            <section class="rooms">
                {#each categories as category}
                    <!-- Category header row — click to collapse/expand -->
                    <div
                        class="sub-space"
                        role="button"
                        tabindex="0"
                        onclick={() => toggleCategory(category.name)}
                        onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && toggleCategory(category.name)}
                    >
                        <img
                            src="/dropdown.svg"
                            class="sub-space-arrow"
                            class:sub-space-arrow-collapsed={collapsed[category.name]}
                            alt=""
                        />
                        <span>{category.name}</span>
                    </div>

                    {#if !collapsed[category.name]}
                        {#each category.rooms as room}
                            <div
                                class="room"
                                class:active={room.name === activeRoom && room.type === 'text'}
                                class:voice-room={room.type === 'voice'}
                                class:voice-room-active={room.type === 'voice' && activeVoiceRoom === room.name}
                                role="button"
                                tabindex="0"
                                onclick={() => room.type === 'voice' && joinVoiceRoom(room.name)}
                                onkeydown={(e) => room.type === 'voice' && (e.key === 'Enter' || e.key === ' ') && joinVoiceRoom(room.name)}
                            >
                                <img
                                    src={room.type === 'voice' ? '/voicechat.svg' : room.encrypted ? '/encrypted.svg' : '/unencrypted.svg'}
                                    class="room-icon"
                                    alt=""
                                />
                                <span>{room.name}</span>
                            </div>

                            <!-- Participant list expands below active voice rooms -->
                            {#if room.type === 'voice' && activeVoiceRoom === room.name}
                                <div class="voice-participants">
                                    {#each voiceParticipants as participant}
                                        <div class="voice-participant">
                                            <img src="/undeafened.svg" class="voice-participant-pfp" alt="" />
                                            <span>{participant}</span>
                                        </div>
                                    {/each}
                                </div>
                            {/if}
                        {/each}
                    {/if}
                {/each}
            </section>

        </aside>
        </div>

    </div><!-- end .left-panel-columns -->

    <!-- Persistent pfp: sits absolutely at bottom-left of left-panel, never inside
         any scaleX transform, so it stays crisp in both collapsed and expanded states -->
    <div class="persistent-pfp">
        <div class="pfp-placeholder"></div>
        <div class="activity-status online"></div>
    </div>

    <!-- User bar: shown at the bottom of the left panel; hidden when collapsed -->
    <footer class="user-bar" class:in-call={activeVoiceRoom !== null}>

        <!-- Voice call info strip (only visible while in a voice room) -->
        {#if activeVoiceRoom !== null}
            <div class="voice-call-info">
                <div class="voice-call-status">
                    <img src="/bars.svg" class="voice-call-icon" alt="" />
                    <span class="voice-call-label">connected</span>
                </div>
                <div class="voice-call-bottom">
                    <span class="voice-call-room">{activeVoiceRoom}</span>
                    <div class="voice-call-actions">
                        <button class="voice-action-btn" title="share screen">
                            <img src="/screenshare.svg" class="voice-action-icon" alt="screen share" />
                        </button>
                        <button class="voice-action-btn" title="start video">
                            <img src="/video.svg" class="voice-action-icon" alt="video" />
                        </button>
                    </div>
                </div>
            </div>
            <div class="voice-call-divider"></div>
        {/if}

        <!-- Profile info + audio controls -->
        <div class="user-bar-main">
            <div class="user-bar-info">
                <span class="user-bar-name">{username}</span>
                <span class="user-bar-homeserver">{homeserver}</span>
            </div>
            <div class="user-bar-controls">
                <button
                    class="control-btn"
                    class:active={muted}
                    onclick={() => muted = !muted}
                    title={muted ? "unmute" : "mute"}
                >
                    <img src={muted ? "/muted.svg" : "/unmuted.svg"} alt="mute" />
                </button>
                <button
                    class="control-btn"
                    class:active={deafened}
                    onclick={() => deafened = !deafened}
                    title={deafened ? "undeafen" : "deafen"}
                >
                    <img src={deafened ? "/deafened.svg" : "/undeafened.svg"} class="flip-h" alt="deafen" />
                </button>
            </div>
        </div>

    </footer>

    </div><!-- end .left-panel -->

    <!-- ── Main content area ── -->
    <main class="app-layout">

        <!-- Chat column -->
        <section class="chat-main">

            <header class="chat-header section-header-row gapped-line-bottom">
                <div class="chat-header-left">
                    <img src="/encrypted.svg" class="chat-header-room-icon" alt="encrypted room" />
                    <span class="chat-header-room-name">{activeRoom}</span>
                </div>
                <div class="chat-header-right">
                    <button class="header-icon-btn" title="pin">
                        <img src="/pin.svg" alt="pin" class="header-empty-icon" />
                    </button>
                    <button
                        class="header-icon-btn"
                        title="members"
                        onclick={() => participantsPanelVisible = !participantsPanelVisible}
                    >
                        <img src="/members.svg" alt="members" class="header-empty-icon" />
                    </button>
                </div>
            </header>

            <!-- Message feed -->
            <section class="chat-messages">
                {#each messages as message}
                    <article class="message">
                        <div class="message-avatar"></div>
                        <div class="message-content">
                            {#if message.repliedTo}
                                <div class="message-reply-link">
                                    <img src="/reply.svg" class="reply-link-icon" alt="" />
                                    <span>{message.repliedTo}</span>
                                </div>
                            {/if}
                            <div class="message-meta">
                                <span class="message-author">{message.user}</span>
                                <span class="message-time">{message.time}</span>
                            </div>
                            <p class="message-text">{message.text}</p>
                            {#if message.image}
                                <div class="message-image-placeholder" aria-label="image placeholder">
                                    <span>LET IT BURN</span>
                                </div>
                            {/if}
                        </div>
                        <button class="message-reply-btn" title="reply">
                            <img src="/reply.svg" class="reply-icon" alt="reply" />
                        </button>
                    </article>
                {/each}
            </section>

            <!-- Composer -->
            <footer class="chat-composer-area">
                <div class="typing-indicator"> ... <span>flaxeneel2 is typing</span></div>
                <div class="chat-composer">
                    <button class="composer-icon-btn" title="add media">
                        <img src="/attach.svg" alt="attach" class="header-empty-icon" />
                    </button>
                    <textarea rows=1 wrap="hard" maxlength=8000 placeholder="enter a message..." class="chat-input"></textarea>
                    <button class="composer-send-btn" title="send">
                        <img src="/send.svg" alt="send" class="header-empty-icon" />
                    </button>
                </div>
            </footer>

        </section>

        <!-- Participants panel shell: width collapse is isolated to this wrapper -->
        <div class="participants-shell" class:hidden={!participantsPanelVisible}>

        <!-- Participants panel content slides/fades independently of layout width -->
        <aside class="participants-panel">

            <header class="participants-header section-header-row gapped-line-bottom">
                <div class="room-stat room-stat--centered">
                    <span class="room-stat-label">ONLINE</span>
                    <span class="room-stat-value">[ <span class="room-stat-num">72k</span> ]</span>
                </div>
                <div class="room-stat">
                    <span class="room-stat-label">ACTIVITY</span>
                    <span class="room-stat-bar" style="--fill: 0.72"></span>
                    <span class="room-stat-pct">72%</span>
                </div>
            </header>

            <section class="participants-list">
                {#each participantGroups as group}
                    <div class="participants-group">
                        <div
                            class="participants-group-title"
                            role="button"
                            tabindex="0"
                            onclick={() => toggleGroup(group.title)}
                            onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && toggleGroup(group.title)}
                        >
                            <span>{group.title}</span>
                            <img
                                src="/dropdown.svg"
                                class="participants-group-arrow"
                                class:participants-group-arrow-collapsed={collapsedGroups[group.title]}
                                alt="toggle group"
                            />
                        </div>
                        {#if !collapsedGroups[group.title]}
                            {#each group.users as member}
                                <div class="participants-member">
                                    <div class="message-avatar"></div>
                                    <span>{member}</span>
                                </div>
                            {/each}
                        {/if}
                    </div>
                {/each}
            </section>

        </aside>
        </div>

    </main>

</div>
