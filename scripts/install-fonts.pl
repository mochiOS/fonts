#!/usr/bin/env perl

use strict;
use warnings;

use File::Basename qw(dirname basename);
use File::Copy qw(copy);
use File::Path qw(make_path remove_tree);
use File::Spec;
use Getopt::Long qw(GetOptions);

my $config = '';
my $output = '';
my $cache = '';
my $work = '';
my $curl = 'curl';
my $list = 0;

GetOptions(
    'config=s' => \$config,
    'output=s' => \$output,
    'cache=s'  => \$cache,
    'work=s'   => \$work,
    'curl=s'   => \$curl,
    'list'     => \$list,
) or die usage();

die "--config is required\n" if $config eq '';

my @fonts = read_config($config);

if ($list) {
    print_font_list(@fonts);
    exit 0;
}

die "--output is required\n" if $output eq '';
die "--cache is required\n" if $cache eq '';
die "--work is required\n" if $work eq '';

make_path($output);
make_path($cache);
make_path($work);
make_path(File::Spec->catdir($output, 'licenses'));

my %archives;

for my $font (@fonts) {
    my $archive_key = join "\0",
        $font->{family},
        $font->{version},
        $font->{url};

    if (!exists $archives{$archive_key}) {
        $archives{$archive_key} = prepare_archive(
            font  => $font,
            cache => $cache,
            work  => $work,
            curl  => $curl,
        );
    }

    install_font(
        font       => $font,
        extracted  => $archives{$archive_key},
        output     => $output,
    );

    install_license(
        font      => $font,
        extracted => $archives{$archive_key},
        output    => $output,
        cache     => $cache,
        curl      => $curl,
    );
}

print "Installed ", scalar(@fonts), " font files into $output\n";

sub usage {
    return <<'USAGE';
usage:
  install-fonts.pl --config fonts.conf --output DIR --cache DIR --work DIR
  install-fonts.pl --config fonts.conf --list
USAGE
}

sub read_config {
    my ($path) = @_;

    open my $handle, '<', $path
        or die "failed to open config '$path': $!\n";

    my @entries;
    my $line_number = 0;

    while (my $line = <$handle>) {
        ++$line_number;

        chomp $line;
        $line =~ s/\r$//;
        $line =~ s/^\s+//;
        $line =~ s/\s+$//;

        next if $line eq '';
        next if $line =~ /^#/;

        my @fields = split /\|/, $line, -1;

        if (@fields != 7) {
            die "$path:$line_number: expected 7 fields, got "
                . scalar(@fields)
                . "\n";
        }

        my (
            $family,
            $version,
            $url,
            $archive_type,
            $source_path,
            $output_name,
            $license_path,
        ) = @fields;

        for my $field (
            $family,
            $version,
            $url,
            $archive_type,
            $source_path,
            $output_name,
            $license_path,
        ) {
            die "$path:$line_number: empty field\n"
                if $field eq '';
        }

        die "$path:$line_number: unsupported archive type '$archive_type'\n"
            if $archive_type ne 'zip'
            && $archive_type ne 'tar.gz'
            && $archive_type ne 'tar.xz';

        die "$path:$line_number: invalid output name '$output_name'\n"
            if basename($output_name) ne $output_name;

        push @entries, {
            family       => $family,
            version      => $version,
            url          => $url,
            archive_type => $archive_type,
            source_path  => $source_path,
            output_name  => $output_name,
            license_path => $license_path,
        };
    }

    close $handle
        or die "failed to close config '$path': $!\n";

    return @entries;
}

sub print_font_list {
    my (@entries) = @_;

    for my $font (@entries) {
        printf "%-18s %-10s %s\n",
            $font->{family},
            $font->{version},
            $font->{output_name};
    }
}

sub prepare_archive {
    my (%args) = @_;

    my $font = $args{font};
    my $cache_dir = $args{cache};
    my $work_dir = $args{work};
    my $curl_command = $args{curl};

    my $archive_name = archive_name($font);
    my $archive_path = File::Spec->catfile(
        $cache_dir,
        $archive_name,
    );

    my $extract_dir = File::Spec->catdir(
        $work_dir,
        sanitize_name($font->{family} . '-' . $font->{version}),
    );

    if (!-f $archive_path) {
        print "Downloading $font->{family} $font->{version}\n";

        run_command(
            $curl_command,
            '--fail',
            '--location',
            '--retry',
            '3',
            '--output',
            $archive_path,
            $font->{url},
        );
    }

    remove_tree($extract_dir) if -d $extract_dir;
    make_path($extract_dir);

    print "Extracting $archive_name\n";

    if ($font->{archive_type} eq 'zip') {
        run_command(
            'unzip',
            '-q',
            $archive_path,
            '-d',
            $extract_dir,
        );
    } elsif ($font->{archive_type} eq 'tar.gz') {
        run_command(
            'tar',
            '-xzf',
            $archive_path,
            '-C',
            $extract_dir,
        );
    } elsif ($font->{archive_type} eq 'tar.xz') {
        run_command(
            'tar',
            '-xJf',
            $archive_path,
            '-C',
            $extract_dir,
        );
    } else {
        die "unsupported archive type '$font->{archive_type}'\n";
    }

    return $extract_dir;
}

sub install_font {
    my (%args) = @_;

    my $font = $args{font};
    my $extract_dir = $args{extracted};
    my $output_dir = $args{output};

    my $source = find_archive_file(
        $extract_dir,
        $font->{source_path},
    );

    my $destination = File::Spec->catfile(
        $output_dir,
        $font->{output_name},
    );

    copy($source, $destination)
        or die "failed to copy '$source' to '$destination': $!\n";

    print "Installed $font->{output_name}\n";
}

sub install_license {
    my (%args) = @_;

    my $font = $args{font};
    my $extract_dir = $args{extracted};
    my $output_dir = $args{output};
    my $cache_dir = $args{cache};
    my $curl_command = $args{curl};

    my $license_name = sanitize_name(
        $font->{family}
        . '-'
        . $font->{version}
        . '-LICENSE.txt'
    );

    my $destination = File::Spec->catfile(
        $output_dir,
        'licenses',
        $license_name,
    );

    return if -f $destination;

    if ($font->{license_path} =~ m{^https?://}) {
        my $cached_license = File::Spec->catfile(
            $cache_dir,
            $license_name,
        );

        if (!-f $cached_license) {
            print "Downloading license for $font->{family} $font->{version}\n";

            run_command(
                $curl_command,
                '--fail',
                '--location',
                '--retry',
                '3',
                '--output',
                $cached_license,
                $font->{license_path},
            );
        }

        copy($cached_license, $destination)
            or die "failed to copy '$cached_license' to '$destination': $!\n";

        return;
    }

    my $source = find_archive_file(
        $extract_dir,
        $font->{license_path},
    );

    copy($source, $destination)
        or die "failed to copy '$source' to '$destination': $!\n";
}

sub find_archive_file {
    my ($root, $relative_path) = @_;

    my $direct = File::Spec->catfile(
        $root,
        split m{/}, $relative_path,
    );

    return $direct if -f $direct;

    my $target_name = basename($relative_path);
    my @matches;

    walk_directory(
        $root,
        sub {
            my ($path) = @_;
            push @matches, $path
                if basename($path) eq $target_name;
        },
    );

    if (@matches == 0) {
        die "file '$relative_path' was not found under '$root'\n";
    }

    if (@matches > 1) {
        my $normalized_suffix = normalize_path($relative_path);
        my @suffix_matches = grep {
            my $normalized = normalize_path($_);
            $normalized =~ /\Q$normalized_suffix\E$/
        } @matches;

        @matches = @suffix_matches if @suffix_matches == 1;
    }

    if (@matches != 1) {
        die "file '$relative_path' is ambiguous under '$root':\n  "
            . join("\n  ", @matches)
            . "\n";
    }

    return $matches[0];
}

sub walk_directory {
    my ($directory, $callback) = @_;

    opendir my $handle, $directory
        or die "failed to open directory '$directory': $!\n";

    my @entries = grep {
        $_ ne '.'
        && $_ ne '..'
    } readdir $handle;

    closedir $handle
        or die "failed to close directory '$directory': $!\n";

    for my $entry (@entries) {
        my $path = File::Spec->catfile($directory, $entry);

        if (-d $path) {
            walk_directory($path, $callback);
        } elsif (-f $path) {
            $callback->($path);
        }
    }
}

sub archive_name {
    my ($font) = @_;

    my $extension = $font->{archive_type} eq 'zip'
        ? 'zip'
        : $font->{archive_type};

    return sanitize_name(
        $font->{family}
        . '-'
        . $font->{version}
        . '.'
        . $extension
    );
}

sub sanitize_name {
    my ($name) = @_;

    $name =~ s/[^A-Za-z0-9._-]+/-/g;
    $name =~ s/^-+//;
    $name =~ s/-+$//;

    return $name;
}

sub normalize_path {
    my ($path) = @_;

    $path =~ s{\\}{/}g;
    $path =~ s{/+}{/}g;

    return $path;
}

sub run_command {
    my (@command) = @_;

    system @command;

    if ($? == -1) {
        die "failed to execute '$command[0]': $!\n";
    }

    if ($? & 127) {
        die sprintf(
            "command '%s' terminated by signal %d\n",
            $command[0],
            $? & 127,
        );
    }

    my $exit_code = $? >> 8;

    die "command '$command[0]' exited with status $exit_code\n"
        if $exit_code != 0;
}